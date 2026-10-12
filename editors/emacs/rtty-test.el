;;; rtty-test.el --- Tests for rtty.el  -*- lexical-binding: t; -*-

;;; Commentary:

;; Against the stub editors/tests/rtty, which logs each call's argv:
;;
;;   emacs --batch -l ert -l <abs>/rtty.el -l <abs>/rtty-test.el \
;;     -f ert-run-tests-batch-and-exit

;;; Code:

(require 'cl-lib)
(require 'ert)
(require 'rtty)

(defconst rtty-test--stub
  (expand-file-name "../tests/rtty" (file-name-directory (or load-file-name buffer-file-name))))

(defun rtty-test--calls (answers thunk)
  "Call THUNK in pane 7 with the stub as `rtty-program'; the stub's calls.
ANSWERS gives the control files for a call by its number, as in
\((2 (\"exit\" . \"1\")))."
  (let* ((dir (make-temp-file "rtty-stub" t))
         (log (expand-file-name "log" dir))
         (process-environment (append (list (concat "RTTY_STUB=" dir) "RTTY_PANE=7")
                                      process-environment))
         (rtty-program rtty-test--stub))
    (setenv "TUTTI_PANE")
    (unwind-protect
        (progn
          (pcase-dolist (`(,n . ,files) answers)
            (pcase-dolist (`(,name . ,content) files)
              (with-temp-file (format "%s/%d.%s" dir n name)
                (insert content))))
          (funcall thunk)
          (when (file-exists-p log)
            (with-temp-buffer
              (insert-file-contents log)
              (split-string (buffer-string) "\n" t))))
      (delete-directory dir t))))

(defun rtty-test--sent (thunk)
  "The strings THUNK sends to the terminal."
  (let (sent)
    (cl-letf (((symbol-function 'send-string-to-terminal)
               (lambda (string &rest _) (push string sent))))
      (funcall thunk))
    (nreverse sent)))

(defconst rtty-test--claim "\e]1337;SetUserVar=RTTY_KEYS=cGFuZQ==\a")
(defconst rtty-test--release "\e]1337;SetUserVar=RTTY_KEYS=\a")

(ert-deftest rtty-mode-claims-releases-and-hooks ()
  (let ((process-environment (cons "RTTY_PANE=7" process-environment)))
    (setenv "TUTTI_PANE")
    (should (equal (rtty-test--sent
                    (lambda ()
                      (rtty-mode 1)
                      (should (memq #'rtty--release kill-emacs-hook))
                      (run-hooks 'suspend-hook)
                      (run-hooks 'suspend-resume-hook)
                      (rtty-mode -1)
                      (should-not (memq #'rtty--release kill-emacs-hook))
                      (should-not (memq #'rtty--release suspend-hook))
                      (should-not (memq #'rtty--claim suspend-resume-hook))))
                   (list rtty-test--claim rtty-test--release rtty-test--claim rtty-test--release)))))

(ert-deftest rtty-mode-is-inert-outside-a-pane-terminal-and-inside-tutti ()
  (let ((process-environment (copy-sequence process-environment)))
    (setenv "RTTY_PANE")
    (setenv "TUTTI_PANE")
    (should-not (rtty-test--sent #'rtty--claim))
    (setenv "RTTY_PANE" "7")
    (cl-letf (((symbol-function 'display-graphic-p) #'always))
      (should-not (rtty-test--sent #'rtty--claim)))
    (setenv "TUTTI_PANE" "3")
    (should-not (rtty-test--sent #'rtty--claim))))

(ert-deftest rtty-windmove-moves-within-then-past-the-edge ()
  (delete-other-windows)
  (let ((left (selected-window))
        (right (split-window-right)))
    (should (equal (rtty-test--calls
                    '((2 ("exit" . "1")))
                    (lambda ()
                      (rtty-windmove-right)
                      (should (eq (selected-window) right))
                      (should (rtty-windmove-right))
                      (should-not (rtty-windmove-up))
                      (rtty-windmove-left)
                      (should (eq (selected-window) left))
                      (rtty-windmove-down)))
                   '("[cli][pane][focus][--direction][right][--from][7]"
                     "[cli][pane][focus][--direction][up][--from][7]"
                     "[cli][pane][focus][--direction][down][--from][7]"))))
  (delete-other-windows))

(ert-deftest rtty-windmove-never-signals-in-a-pane ()
  (delete-other-windows)
  (let ((rtty-program "/nonexistent/rtty")
        (process-environment (cons "RTTY_PANE=7" process-environment)))
    (setenv "TUTTI_PANE")
    (should-not (rtty-windmove-left))))

(ert-deftest rtty-windmove-outside-a-pane-or-inside-tutti-is-windmove ()
  (delete-other-windows)
  (should (equal (rtty-test--calls
                  nil
                  (lambda ()
                    (setenv "TUTTI_PANE" "3")
                    (should-error (rtty-windmove-left) :type 'user-error)
                    (setenv "TUTTI_PANE")
                    (setenv "RTTY_PANE")
                    (should-error (rtty-windmove-left) :type 'user-error)))
                 nil)))

(ert-deftest rtty-resize-moves-rttys-split ()
  (should (equal (rtty-test--calls
                  nil
                  (lambda ()
                    (rtty-resize-left)
                    (rtty-resize-down)))
                 '("[cli][pane][resize][--direction][left][--from][7]"
                   "[cli][pane][resize][--direction][down][--from][7]"))))

(provide 'rtty-test)
;;; rtty-test.el ends here
