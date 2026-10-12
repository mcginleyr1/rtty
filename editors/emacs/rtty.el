;;; rtty.el --- Emacs in an rtty pane  -*- lexical-binding: t; -*-

;; Version: 0.1.0
;; Package-Requires: ((emacs "29.1"))
;; Keywords: terminals, tools

;;; Commentary:

;; For Emacs in a terminal frame in an rtty pane ($RTTY_PANE set, and not in tutti inside it,
;; $TUTTI_PANE, whose own plugin owns navigation there):
;;
;; - `rtty-mode' claims rtty's focus and resize keys, so with rtty's `keys = "tutti"' Ctrl+h/j/k/l
;;   and Alt+h/j/k/l reach Emacs.  rtty's Cmd chords never do: Emacs decodes no Cmd modifier
;;   from a terminal, so rtty keeps acting on them.
;; - `rtty-windmove-left' and friends move between windows, and past the frame's edge on to
;;   rtty's panes.
;; - `rtty-resize-left' and friends move rtty's split border.

;;; Code:

(require 'windmove)

(defgroup rtty nil
  "Emacs in an rtty pane."
  :group 'tools
  :prefix "rtty-")

(defcustom rtty-program "rtty"
  "The rtty executable, which `rtty cli' runs."
  :type 'string)

(defun rtty--pane ()
  "This Emacs's rtty pane, unless it runs in tutti inside rtty."
  (and (not (getenv "TUTTI_PANE")) (getenv "RTTY_PANE")))

;;;; Claiming rtty's keys

(defun rtty--user-var (name value)
  "The OSC 1337 sequence setting the user var NAME to VALUE; empty clears it."
  (format "\e]1337;SetUserVar=%s=%s\a"
          name (base64-encode-string (encode-coding-string value 'utf-8) t)))

(defun rtty--send-keys (value)
  "Set rtty's RTTY_KEYS to VALUE.
Only a terminal frame in an rtty pane has an rtty to tell."
  (when (and (rtty--pane) (not (display-graphic-p)))
    (send-string-to-terminal (rtty--user-var "RTTY_KEYS" value))))

(defun rtty--claim ()
  "Tell rtty this Emacs takes its keys."
  (rtty--send-keys "pane"))

(defun rtty--release ()
  "Give rtty its keys back."
  (rtty--send-keys ""))

;;;###autoload
(define-minor-mode rtty-mode
  "Tell rtty that Emacs takes its focus and resize keys.
While on, rtty passes them to Emacs instead of acting on them: with
rtty's `keys = \"tutti\"', Ctrl+h/j/k/l and Alt+h/j/k/l.  Released
while Emacs is suspended and when it exits.  Does nothing outside a
terminal frame in an rtty pane."
  :global t
  :group 'rtty
  (let ((hook (if rtty-mode #'add-hook #'remove-hook)))
    (funcall hook 'kill-emacs-hook #'rtty--release)
    (funcall hook 'suspend-hook #'rtty--release)
    (funcall hook 'suspend-resume-hook #'rtty--claim))
  (if rtty-mode (rtty--claim) (rtty--release)))

;;;; Moving between windows and panes

(defun rtty--cli (&rest args)
  "Run `rtty cli' with ARGS; non-nil when it exited 0.  Never signals."
  (eql 0 (ignore-errors (apply #'call-process rtty-program nil nil nil "cli" args))))

(defun rtty--windmove (direction)
  "Select the window in DIRECTION, or past the frame's edge, rtty's pane.
The edge is wherever windmove refuses: no window there, or only the
inactive minibuffer.  Outside an rtty pane, windmove's refusal stands."
  (condition-case refusal
      (windmove-do-window-select direction)
    (error
     (let ((pane (or (rtty--pane) (signal (car refusal) (cdr refusal)))))
       (rtty--cli "pane" "focus" "--direction" (symbol-name direction) "--from" pane)))))

;;;###autoload
(defun rtty-windmove-left ()
  "Select the window to the left, or past the frame's edge, rtty's pane."
  (interactive)
  (rtty--windmove 'left))

;;;###autoload
(defun rtty-windmove-right ()
  "Select the window to the right, or past the frame's edge, rtty's pane."
  (interactive)
  (rtty--windmove 'right))

;;;###autoload
(defun rtty-windmove-up ()
  "Select the window above, or past the frame's edge, rtty's pane."
  (interactive)
  (rtty--windmove 'up))

;;;###autoload
(defun rtty-windmove-down ()
  "Select the window below, or past the frame's edge, rtty's pane."
  (interactive)
  (rtty--windmove 'down))

;;;; Resizing rtty's split

(defun rtty--resize (direction)
  "Move rtty's split border nearest this pane toward DIRECTION by rtty's step."
  (when-let* ((pane (rtty--pane)))
    (rtty--cli "pane" "resize" "--direction" (symbol-name direction) "--from" pane)))

;;;###autoload
(defun rtty-resize-left ()
  "Move rtty's split border nearest this pane to the left."
  (interactive)
  (rtty--resize 'left))

;;;###autoload
(defun rtty-resize-right ()
  "Move rtty's split border nearest this pane to the right."
  (interactive)
  (rtty--resize 'right))

;;;###autoload
(defun rtty-resize-up ()
  "Move rtty's split border nearest this pane up."
  (interactive)
  (rtty--resize 'up))

;;;###autoload
(defun rtty-resize-down ()
  "Move rtty's split border nearest this pane down."
  (interactive)
  (rtty--resize 'down))

(provide 'rtty)
;;; rtty.el ends here
