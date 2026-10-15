use std::{env, path::PathBuf};

/// On Windows the profile folder wins over `HOME`: Claude Code (Node, Bun)
/// and Codex ignore `HOME`, which Git Bash, Cygwin or Emacs may point elsewhere.
pub(crate) fn home_dir() -> Option<PathBuf> {
    home_dir_from_env(
        cfg!(windows),
        env::var_os("HOME"),
        env::var_os("USERPROFILE"),
        env::var_os("HOMEDRIVE"),
        env::var_os("HOMEPATH"),
    )
}

fn home_dir_from_env(
    profile_first: bool,
    home: Option<std::ffi::OsString>,
    user_profile: Option<std::ffi::OsString>,
    home_drive: Option<std::ffi::OsString>,
    home_path: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let home = non_empty_path(home);
    if !profile_first && home.is_some() {
        return home;
    }
    non_empty_path(user_profile)
        .or_else(|| drive_path(home_drive, home_path))
        .or(home)
}

fn drive_path(
    home_drive: Option<std::ffi::OsString>,
    home_path: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let mut drive = home_drive?;
    let path = home_path?;
    if drive.is_empty() || path.is_empty() {
        return None;
    }
    drive.push(path);
    Some(PathBuf::from(drive))
}

fn non_empty_path(path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let path = path?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn prefers_home_when_available() {
        let path = home_dir_from_env(
            false,
            Some(OsString::from("/home/user")),
            Some(OsString::from("C:\\Users\\runner")),
            None,
            None,
        );
        assert_eq!(path, Some(PathBuf::from("/home/user")));
    }

    #[test]
    fn falls_back_to_windows_user_profile_without_home() {
        let path = home_dir_from_env(
            false,
            None,
            Some(OsString::from("C:\\Users\\runner")),
            None,
            None,
        );
        assert_eq!(path, Some(PathBuf::from("C:\\Users\\runner")));
    }

    #[test]
    fn falls_back_to_windows_home_drive_and_path() {
        let path = home_dir_from_env(
            false,
            None,
            None,
            Some(OsString::from("C:")),
            Some(OsString::from("\\Users\\runner")),
        );
        assert_eq!(path, Some(PathBuf::from("C:\\Users\\runner")));
    }

    #[test]
    fn windows_prefers_user_profile_over_home() {
        let path = home_dir_from_env(
            true,
            Some(OsString::from("C:\\msys64\\home\\runner")),
            Some(OsString::from("C:\\Users\\runner")),
            None,
            None,
        );
        assert_eq!(path, Some(PathBuf::from("C:\\Users\\runner")));
    }

    #[test]
    fn windows_prefers_home_drive_and_path_over_home() {
        let path = home_dir_from_env(
            true,
            Some(OsString::from("C:\\msys64\\home\\runner")),
            None,
            Some(OsString::from("C:")),
            Some(OsString::from("\\Users\\runner")),
        );
        assert_eq!(path, Some(PathBuf::from("C:\\Users\\runner")));
    }

    #[test]
    fn windows_falls_back_to_home_without_profile() {
        let path = home_dir_from_env(
            true,
            Some(OsString::from("C:\\msys64\\home\\runner")),
            None,
            None,
            None,
        );
        assert_eq!(path, Some(PathBuf::from("C:\\msys64\\home\\runner")));
    }
}
