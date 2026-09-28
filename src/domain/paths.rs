use std::path::{Path, PathBuf};

use crate::error::AppError;

/// Where wt reads its config and writes its log. Both fall under one
/// directory when `WT_HOME` is set.
#[derive(Debug, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub log_dir: PathBuf,
}

/// Resolves wt's directories from `home` and an environment lookup.
/// `WT_HOME`, when non-empty, puts both under the one directory it names.
/// Otherwise: `<home>/.config/wt` and `<home>/.local/state/wt`.
pub fn resolve(
    home: Option<&Path>,
    var: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Paths, AppError> {
    if let Some(dir) = var("WT_HOME").filter(|p| !p.as_os_str().is_empty()) {
        return Ok(Paths {
            config_dir: dir.clone(),
            log_dir: dir,
        });
    }
    let home = home.ok_or(AppError::NoHome)?;
    Ok(Paths {
        config_dir: home.join(".config").join("wt"),
        log_dir: home.join(".local").join("state").join("wt"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| PathBuf::from(v))
        }
    }

    // Absolute on both Windows and Unix.
    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{p}"))
        } else {
            PathBuf::from(p)
        }
    }

    #[test]
    fn defaults_under_home() {
        let home = abs("/home/bk");
        let paths = resolve(Some(&home), env(&[])).unwrap();
        assert_eq!(
            paths,
            Paths {
                config_dir: home.join(".config").join("wt"),
                log_dir: home.join(".local").join("state").join("wt"),
            }
        );
    }

    #[test]
    fn wt_home_overrides_both() {
        let t = abs("/tmp/t");
        let vars = [("WT_HOME", t.to_str().unwrap())];
        let paths = resolve(None, env(&vars)).unwrap();
        assert_eq!(
            paths,
            Paths {
                config_dir: t.clone(),
                log_dir: t,
            }
        );
    }

    #[test]
    fn empty_wt_home_is_ignored() {
        let home = abs("/home/bk");
        let paths = resolve(Some(&home), env(&[("WT_HOME", "")])).unwrap();
        assert_eq!(paths.config_dir, home.join(".config").join("wt"));
    }

    #[test]
    fn no_home_and_no_wt_home_is_an_error() {
        assert!(matches!(resolve(None, env(&[])), Err(AppError::NoHome)));
    }
}
