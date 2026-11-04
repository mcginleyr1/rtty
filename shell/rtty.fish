# rtty's shell integration for fish, which finds it in vendor_conf.d through the directory rtty
# puts first in XDG_DATA_DIRS. It puts XDG_DATA_DIRS back at once, so the shell's children (another
# fish, the shells tutti starts) inherit nothing of rtty's. Not yet tried with a real fish.
if set -q RTTY_XDG_DATA_DIRS
    set -gx XDG_DATA_DIRS $RTTY_XDG_DATA_DIRS
    set -e RTTY_XDG_DATA_DIRS
else
    set -e XDG_DATA_DIRS
end

# At each prompt the directory (OSC 7), and the prompt's start (A) and end (B) around fish_prompt,
# wrapped at the first prompt, once the user's own is defined. Before a command where its output
# starts (C), after it its end and status (D).
if status is-interactive
    function __rtty_prompt --on-event fish_prompt
        # Each part of the path escaped on its own, so the slashes between them stay slashes
        # whatever the URL escaping does with one.
        set -l path ''
        for part in (string split --no-empty / -- $PWD)
            set path "$path/"(string escape --style=url -- $part)
        end
        test -n "$path"; or set path /
        printf '\e]7;file://%s%s\a' $hostname $path
        functions -q __rtty_user_prompt; and return
        functions -c fish_prompt __rtty_user_prompt
        # The user's prompt runs first, while $status and $pipestatus are still the command's, and
        # its output goes out between the marks whole, a last newline (input on a line of its
        # own) included.
        function fish_prompt
            set -l prompt (__rtty_user_prompt | string collect --no-trim-newlines)
            printf '\e]133;A\a%s\e]133;B\a' $prompt
        end
    end
    function __rtty_preexec --on-event fish_preexec
        string length -q -- $argv[1]; and printf '\e]133;C\a'
    end
    function __rtty_postexec --on-event fish_postexec
        printf '\e]133;D;%s\a' $status
    end
end
