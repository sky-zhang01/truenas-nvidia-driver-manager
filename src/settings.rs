//! The four settings and their validation.
//!
//! All environment variables are read at once; every problem is collected and the process
//! never exits because of one.

use std::ffi::OsString;
use std::fmt;

pub const VAR_USERNAME: &str = "NVM_INITIAL_ADMIN_USERNAME";
pub const VAR_PASSWORD: &str = "NVM_INITIAL_ADMIN_PASSWORD";
pub const VAR_PORT: &str = "NVM_PORT";
pub const VAR_BUILD_TIMEOUT: &str = "NVM_BUILD_TIMEOUT_MINUTES";

pub const DEFAULT_USERNAME: &str = "admin";
pub const DEFAULT_PORT: u16 = 8199;
pub const DEFAULT_BUILD_TIMEOUT_MINUTES: u32 = 30;

/// An initial or new password; its `Debug` output is `<redacted>`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Secret {
        Secret(value)
    }

    /// The only way to read the value; callers must not log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// State of the initial password.
#[derive(Debug, Clone)]
pub enum InitialPassword {
    Usable(Secret),
    Unusable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingProblem {
    Unknown {
        variable: String,
    },
    Invalid {
        variable: &'static str,
        rule: &'static str,
    },
    NotUtf8 {
        variable: String,
    },
}

/// Rule keys of invalid-setting notice (the front end renders the requirement sentence).
pub const RULE_USERNAME: &str = "setting.rule.username";
pub const RULE_PORT: &str = "setting.rule.port";
pub const RULE_BUILD_TIMEOUT: &str = "setting.rule.build_timeout";

#[derive(Debug, Clone)]
pub struct Settings {
    /// `Some` only when the configured name is acceptable; absent means the default. `None`
    /// means the value was set but is invalid.
    pub username: Option<String>,
    pub password: InitialPassword,
    pub port: u16,
    pub build_timeout_minutes: Option<u32>,
    pub problems: Vec<SettingProblem>,
}

impl Settings {
    pub fn usable(&self) -> bool {
        self.problems.is_empty()
    }
}

fn is_known(name: &str) -> bool {
    matches!(
        name,
        VAR_USERNAME | VAR_PASSWORD | VAR_PORT | VAR_BUILD_TIMEOUT
    )
}

/// Read all environment variables and collect every problem.
pub fn load(vars: impl Iterator<Item = (OsString, OsString)>) -> Settings {
    let mut problems = Vec::new();
    let mut username = None;
    let mut username_seen = false;
    let mut password = InitialPassword::Unusable;
    let mut port = DEFAULT_PORT;
    let mut port_seen = false;
    let mut timeout = None;
    let mut timeout_seen = false;

    for (key, value) in vars {
        let key_str = key.to_string_lossy().into_owned();
        if !is_known(&key_str) {
            // Only the four documented settings are our business; an unrelated variable
            // (UTF-8 or not) never blocks the process.
            if key_str.starts_with("NVM_") {
                problems.push(SettingProblem::Unknown { variable: key_str });
            }
            continue;
        }
        // Whether the name was configured is recorded before the UTF-8 conversion: a value
        // that cannot be converted counts as configured and invalid, so the default `admin`
        // is never used for it.
        if key_str == VAR_USERNAME {
            username_seen = true;
        }
        let Some(text) = value.to_str().map(|s| s.to_string()) else {
            problems.push(SettingProblem::NotUtf8 { variable: key_str });
            continue;
        };
        match key_str.as_str() {
            VAR_USERNAME => {
                if check_username(&text) {
                    username = Some(text);
                } else {
                    problems.push(SettingProblem::Invalid {
                        variable: VAR_USERNAME,
                        rule: RULE_USERNAME,
                    });
                }
            }
            VAR_PASSWORD => {
                password = if text.starts_with('<') && text.ends_with('>') {
                    InitialPassword::Unusable
                } else if check_password(&text).is_ok() {
                    InitialPassword::Usable(Secret::new(text))
                } else {
                    InitialPassword::Unusable
                };
            }
            VAR_PORT => {
                port_seen = true;
                match text.parse::<u32>() {
                    Ok(n) if (1..=65535).contains(&n) => port = n as u16,
                    _ => {
                        problems.push(SettingProblem::Invalid {
                            variable: VAR_PORT,
                            rule: RULE_PORT,
                        });
                    }
                }
            }
            VAR_BUILD_TIMEOUT => {
                timeout_seen = true;
                match text.parse::<u32>() {
                    Ok(n) if (1..=1440).contains(&n) => timeout = Some(n),
                    _ => {
                        problems.push(SettingProblem::Invalid {
                            variable: VAR_BUILD_TIMEOUT,
                            rule: RULE_BUILD_TIMEOUT,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if !username_seen {
        username = Some(DEFAULT_USERNAME.to_string());
    }
    if !port_seen {
        port = DEFAULT_PORT;
    }
    if !timeout_seen {
        timeout = None;
    }

    Settings {
        username,
        password,
        port,
        build_timeout_minutes: timeout,
        problems,
    }
}

/// 1–64 ASCII characters, first `[A-Za-z0-9]`, rest `[A-Za-z0-9._-]`.
pub fn check_username(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 64 {
        return false;
    }
    if !(b[0].is_ascii_alphanumeric()) {
        return false;
    }
    b[1..]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordRule {
    TooShort,
    TooLong,
    Whitespace,
    Placeholder,
    /// The forced first change repeated the current password.
    SameAsInitial,
}

impl PasswordRule {
    pub fn key(self) -> &'static str {
        match self {
            PasswordRule::TooShort => "password.rule.too_short",
            PasswordRule::TooLong => "password.rule.too_long",
            PasswordRule::Whitespace => "password.rule.whitespace",
            PasswordRule::Placeholder => "password.rule.placeholder",
            PasswordRule::SameAsInitial => "password.rule.same_as_initial",
        }
    }
}

/// At least 8 characters, at most 1024 bytes, some non-whitespace, not a `<...>` placeholder;
/// the first unmet rule is reported.
pub fn check_password(s: &str) -> Result<(), PasswordRule> {
    if s.chars().count() < 8 {
        return Err(PasswordRule::TooShort);
    }
    if s.len() > 1024 {
        return Err(PasswordRule::TooLong);
    }
    if s.chars().all(char::is_whitespace) || s.chars().count() == 0 {
        return Err(PasswordRule::Whitespace);
    }
    if s.starts_with('<') && s.ends_with('>') {
        return Err(PasswordRule::Placeholder);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(*k), OsString::from(*v)))
            .collect()
    }

    #[test]
    fn defaults_when_nothing_is_set() {
        let s = load(vars(&[]).into_iter());
        assert_eq!(s.username.as_deref(), Some("admin"));
        assert_eq!(s.port, 8199);
        assert_eq!(s.build_timeout_minutes, None);
        assert!(matches!(s.password, InitialPassword::Unusable));
        assert!(s.problems.is_empty());
    }

    #[test]
    fn valid_values_are_taken() {
        let s = load(
            vars(&[
                ("NVM_INITIAL_ADMIN_USERNAME", "ops.1"),
                ("NVM_INITIAL_ADMIN_PASSWORD", "correct horse"),
                ("NVM_PORT", "9000"),
                ("NVM_BUILD_TIMEOUT_MINUTES", "60"),
            ])
            .into_iter(),
        );
        assert_eq!(s.username.as_deref(), Some("ops.1"));
        assert_eq!(s.port, 9000);
        assert_eq!(s.build_timeout_minutes, Some(60));
        assert!(matches!(s.password, InitialPassword::Usable(_)));
        assert!(s.problems.is_empty());
    }

    #[test]
    fn invalid_values_are_collected_without_stopping() {
        let s = load(
            vars(&[
                ("NVM_FOO", "1"),
                ("NVM_PORT", "abc"),
                ("NVM_BUILD_TIMEOUT_MINUTES", "0"),
                ("NVM_INITIAL_ADMIN_USERNAME", "bad name"),
            ])
            .into_iter(),
        );
        assert_eq!(s.port, 8199);
        assert_eq!(s.build_timeout_minutes, None);
        assert_eq!(s.username, None);
        assert_eq!(s.problems.len(), 4);
        assert!(s.problems.iter().any(|p| matches!(
            p,
            SettingProblem::Unknown { variable } if variable == "NVM_FOO"
        )));
    }

    #[test]
    fn placeholder_and_short_passwords_are_unusable_but_not_problems() {
        let s = load(vars(&[("NVM_INITIAL_ADMIN_PASSWORD", "<initial-password>")]).into_iter());
        assert!(matches!(s.password, InitialPassword::Unusable));
        assert!(s.problems.is_empty());
        let s = load(vars(&[("NVM_INITIAL_ADMIN_PASSWORD", "short")]).into_iter());
        assert!(matches!(s.password, InitialPassword::Unusable));
        assert!(s.problems.is_empty());
    }

    #[test]
    fn a_non_utf8_username_counts_as_configured_and_invalid() {
        use std::os::unix::ffi::OsStringExt;
        let s =
            load(vec![(OsString::from(VAR_USERNAME), OsString::from_vec(vec![0xff]))].into_iter());
        assert_eq!(
            s.username, None,
            "a configured invalid name must not fall back to admin"
        );
        assert!(s.problems.iter().any(
            |p| matches!(p, SettingProblem::NotUtf8 { variable } if variable == VAR_USERNAME)
        ));
    }

    #[test]
    fn username_rules() {
        assert!(check_username("admin"));
        assert!(check_username("A-1._"));
        assert!(!check_username(""));
        assert!(!check_username("-admin"));
        assert!(!check_username("has space"));
        assert!(!check_username(&"a".repeat(65)));
        assert!(check_username(&format!("a{}", "b".repeat(63))));
    }

    #[test]
    fn password_rules() {
        assert_eq!(check_password("12345678"), Ok(()));
        assert_eq!(check_password("1234567"), Err(PasswordRule::TooShort));
        assert_eq!(
            check_password(&"a".repeat(1025)),
            Err(PasswordRule::TooLong)
        );
        assert_eq!(check_password("        "), Err(PasswordRule::Whitespace));
        assert_eq!(check_password("<12345678>"), Err(PasswordRule::Placeholder));
        let long = format!("{}8", " ".repeat(2000));
        assert_eq!(check_password(&long), Err(PasswordRule::TooLong));
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("hunter123".to_string());
        assert_eq!(format!("{s:?}"), "<redacted>");
        assert_eq!(s.expose(), "hunter123");
    }
}
