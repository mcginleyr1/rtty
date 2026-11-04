//! Shell integration that loads itself: rtty's zsh, bash and fish scripts (`shell/`), built in and
//! written at startup into rtty's private directory, and the way each shell is started so that it
//! reads them with no edit to the user's dotfiles. They mark prompts, command lines, output and
//! command ends (OSC 133 A, B, C, D) and report the directory (OSC 7). Each script undoes its
//! injection as it starts, so nothing of it reaches the shell's children.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

const SCRIPTS: [(&str, &str); 3] = [
    ("zsh/.zshenv", include_str!("../shell/rtty.zsh")),
    ("bash/rtty.bash", include_str!("../shell/rtty.bash")),
    ("fish/fish/vendor_conf.d/rtty.fish", include_str!("../shell/rtty.fish")),
];

/// Writes the scripts into `dir/shell`, each through a rename, so a shell starting meanwhile (in
/// another rtty sharing the directory) reads a whole one; the directory they are in.
pub fn install(dir: &Path) -> io::Result<PathBuf> {
    let dir = dir.join("shell");
    for (path, script) in SCRIPTS {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().expect("a script is in a directory"))?;
        let mut partial = path.clone().into_os_string();
        partial.push(format!(".{}", std::process::id()));
        fs::write(&partial, script)?;
        fs::rename(&partial, &path)?;
    }
    Ok(dir)
}

/// Has the login shell `command` starts read the scripts `install` wrote to `dir`: zsh through a
/// ZDOTDIR of rtty's, bash through `--rcfile` (so no longer a login shell, which reads no rc file
/// it is given; the script reads what a login shell would), fish through a vendor directory first
/// in XDG_DATA_DIRS. The user's ZDOTDIR and XDG_DATA_DIRS go along as `RTTY_…`, for the script to
/// put back. Any other shell is left as it is.
pub fn inject(command: &mut CommandBuilder, dir: &Path) {
    let shell = command.get_shell();
    let keep = |command: &mut CommandBuilder, name: &str| {
        let value = command.get_env(name).map(ToOwned::to_owned);
        if let Some(value) = value {
            command.env(format!("RTTY_{name}"), value);
        }
    };
    match shell.rsplit('/').next() {
        Some("zsh") => {
            keep(command, "ZDOTDIR");
            command.env("ZDOTDIR", dir.join("zsh"));
        }
        Some("bash") => {
            let rcfile = dir.join("bash/rtty.bash").into_os_string();
            command.get_argv_mut().extend([OsString::from(shell), "--rcfile".into(), rcfile]);
        }
        Some("fish") => {
            keep(command, "XDG_DATA_DIRS");
            // Unset, it is XDG's default.
            let data =
                command.get_env("XDG_DATA_DIRS").unwrap_or("/usr/local/share:/usr/share".as_ref());
            let mut dirs = dir.join("fish").into_os_string();
            dirs.push(":");
            dirs.push(data);
            command.env("XDG_DATA_DIRS", dirs);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake shells by name, each `/bin/sh` under that name, which `get_shell` takes as a shell.
    fn shells(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rtty-{test}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for name in ["zsh", "bash", "fish", "dash"] {
            let _ = std::os::unix::fs::symlink("/bin/sh", dir.join(name));
        }
        dir
    }

    fn login_shell(path: &Path, env: &[(&str, &str)]) -> CommandBuilder {
        let mut command = CommandBuilder::new_default_prog();
        command.env_clear();
        command.env("SHELL", path);
        for (name, value) in env {
            command.env(name, value);
        }
        command
    }

    #[test]
    fn the_scripts_are_written_whole_where_each_shell_looks() {
        let dir = std::env::temp_dir().join(format!("rtty-install-{}", std::process::id()));
        let shell = install(&dir).unwrap();
        assert_eq!(shell, dir.join("shell"));
        let zshenv = fs::read_to_string(shell.join("zsh/.zshenv")).unwrap();
        assert!(zshenv.contains("RTTY_ZDOTDIR") && zshenv.contains("133;C"));
        assert!(fs::read_to_string(shell.join("bash/rtty.bash")).unwrap().contains("PS0"));
        let fish = shell.join("fish/fish/vendor_conf.d/rtty.fish");
        assert!(fs::read_to_string(fish).unwrap().contains("fish_preexec"));
        // Again over the first, as a second rtty does, leaving no partial file behind.
        install(&dir).unwrap();
        assert_eq!(fs::read_dir(shell.join("zsh")).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zsh_starts_in_rttys_zdotdir_with_the_users_kept() {
        let (bin, dir) = (shells("zdotdir"), Path::new("/run/rtty"));
        let mut own = login_shell(&bin.join("zsh"), &[]);
        inject(&mut own, dir);
        assert!(own.is_default_prog(), "still the login shell");
        assert_eq!(own.get_env("ZDOTDIR"), Some("/run/rtty/zsh".as_ref()));
        assert_eq!(own.get_env("RTTY_ZDOTDIR"), None);
        let mut theirs = login_shell(&bin.join("zsh"), &[("ZDOTDIR", "/home/me/.config/zsh")]);
        inject(&mut theirs, dir);
        assert_eq!(theirs.get_env("ZDOTDIR"), Some("/run/rtty/zsh".as_ref()));
        assert_eq!(theirs.get_env("RTTY_ZDOTDIR"), Some("/home/me/.config/zsh".as_ref()));
        fs::remove_dir_all(bin).unwrap();
    }

    #[test]
    fn bash_reads_rttys_rc_file_and_fish_its_vendor_directory() {
        let (bin, dir) = (shells("rcfile"), Path::new("/run/rtty"));
        let mut bash = login_shell(&bin.join("bash"), &[]);
        inject(&mut bash, dir);
        let bash_path = bin.join("bash").into_os_string();
        let argv = [bash_path, "--rcfile".into(), "/run/rtty/bash/rtty.bash".into()];
        assert_eq!(bash.get_argv(), &argv);
        let mut fish = login_shell(&bin.join("fish"), &[]);
        inject(&mut fish, dir);
        let dirs = fish.get_env("XDG_DATA_DIRS");
        assert_eq!(dirs, Some("/run/rtty/fish:/usr/local/share:/usr/share".as_ref()));
        assert_eq!(fish.get_env("RTTY_XDG_DATA_DIRS"), None);
        let mut theirs = login_shell(&bin.join("fish"), &[("XDG_DATA_DIRS", "/nix/share")]);
        inject(&mut theirs, dir);
        assert_eq!(theirs.get_env("XDG_DATA_DIRS"), Some("/run/rtty/fish:/nix/share".as_ref()));
        assert_eq!(theirs.get_env("RTTY_XDG_DATA_DIRS"), Some("/nix/share".as_ref()));
        let mut dash = login_shell(&bin.join("dash"), &[]);
        inject(&mut dash, dir);
        assert!(dash.is_default_prog() && dash.get_env("ZDOTDIR").is_none());
        fs::remove_dir_all(bin).unwrap();
    }
}
