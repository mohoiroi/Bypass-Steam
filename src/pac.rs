//! Own the app's temporary Windows PAC setting and restore the previous value.

use crate::{config, proxy::PORT};
use std::{ptr::null_mut, sync::atomic::{AtomicUsize, Ordering}};
use windows_sys::Win32::{
    Networking::WinInet::{InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED},
    System::Registry::{REG_EXPAND_SZ, REG_SZ},
};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const AUTO_CONFIG: &str = "AutoConfigURL";
const PREVIOUS_CONFIG: &str = "PreviousAutoConfigURL";
static VERSION: AtomicUsize = AtomicUsize::new(1);

pub struct PacSettings<'a> {
    internet_key: &'a str,
    app_key: &'a str,
}

pub const WINDOWS: PacSettings<'static> = PacSettings {
    internet_key: INTERNET_SETTINGS,
    app_key: config::APP_KEY,
};

#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Clean,
    Restored,
    ChangedExternally,
}

fn pac_prefix() -> String {
    format!("http://127.0.0.1:{PORT}/proxy.pac?v=")
}

fn is_owned(current: &Option<(u32, String)>) -> bool {
    current.as_ref().is_some_and(|(kind, url)| {
        *kind == REG_SZ && url.strip_prefix(&pac_prefix()).is_some_and(|version| {
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
        })
    })
}

fn encode_previous(previous: &Option<(u32, String)>) -> String {
    match previous {
        Some((kind, value)) => format!("{kind}\n{value}"),
        None => "absent".into(),
    }
}

fn decode_previous(saved: &str) -> Result<Option<(u32, &str)>, ()> {
    if saved == "absent" {
        return Ok(None);
    }
    let (kind, value) = saved.split_once('\n').ok_or(())?;
    match kind.parse::<u32>() {
        Ok(kind @ (REG_SZ | REG_EXPAND_SZ)) => Ok(Some((kind, value))),
        _ => Err(()),
    }
}

impl PacSettings<'_> {
    fn publish(&self) -> bool {
        let version = VERSION.fetch_add(1, Ordering::Relaxed);
        let url = format!("{}{version}", pac_prefix());
        config::write_reg_string(self.internet_key, AUTO_CONFIG, REG_SZ, &url)
    }

    pub fn install(&self) -> bool {
        let Ok(current) = config::read_reg_string(self.internet_key, AUTO_CONFIG) else {
            return false;
        };
        // Save before changing Windows settings, including when there was no previous URL.
        let starts_session = !is_owned(&current);
        if starts_session && !config::write_reg_string(self.app_key, PREVIOUS_CONFIG, REG_SZ, &encode_previous(&current)) {
            return false;
        }
        let published = self.publish();
        if !published && starts_session {
            config::delete_reg_value(self.app_key, PREVIOUS_CONFIG);
        }
        published
    }

    pub fn refresh(&self) -> bool {
        // If the user or another app changed the PAC, leave their setting alone.
        config::read_reg_string(self.internet_key, AUTO_CONFIG)
            .is_ok_and(|current| is_owned(&current))
            && self.publish()
    }

    pub fn restore(&self) -> Result<Recovery, ()> {
        let current = config::read_reg_string(self.internet_key, AUTO_CONFIG)?;
        let saved = config::read_reg_string(self.app_key, PREVIOUS_CONFIG)?;
        if !is_owned(&current) {
            return if saved.is_none() {
                Ok(Recovery::Clean)
            } else if config::delete_reg_value(self.app_key, PREVIOUS_CONFIG) {
                Ok(Recovery::ChangedExternally)
            } else {
                Err(())
            };
        }

        // A stale URL from a build without recovery has no previous value to restore.
        let previous = match saved.as_ref() {
            Some((REG_SZ, value)) => decode_previous(value)?,
            Some(_) => return Err(()),
            None => None,
        };
        let restored = match previous {
            Some((kind, url)) => config::write_reg_string(self.internet_key, AUTO_CONFIG, kind, url),
            None => config::delete_reg_value(self.internet_key, AUTO_CONFIG),
        };
        if !restored {
            return Err(());
        }
        config::delete_reg_value(self.app_key, PREVIOUS_CONFIG);
        Ok(Recovery::Restored)
    }
}

pub fn notify_windows() {
    unsafe {
        InternetSetOptionW(null_mut(), INTERNET_OPTION_SETTINGS_CHANGED, null_mut(), 0);
        InternetSetOptionW(null_mut(), INTERNET_OPTION_REFRESH, null_mut(), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_app_pac_and_preserves_previous_value_type() {
        let own = |url: String| is_owned(&Some((REG_SZ, url)));
        assert!(own(format!("{}42", pac_prefix())));
        assert!(!own(pac_prefix()));
        assert!(!own(format!("{}42&keep=1", pac_prefix())));
        assert!(!own("http://corp.example/proxy.pac?v=42".into()));
        assert!(!own(format!("http://127.0.0.1:{PORT}/other.pac?v=42")));

        let previous = Some((REG_EXPAND_SZ, "http://%PAC_HOST%/proxy.pac".into()));
        assert_eq!(
            decode_previous(&encode_previous(&previous)),
            Ok(Some((REG_EXPAND_SZ, "http://%PAC_HOST%/proxy.pac"))),
        );
        assert_eq!(decode_previous(&encode_previous(&None)), Ok(None));
        assert!(decode_previous("broken").is_err());
    }

    #[test]
    fn recovery_restores_previous_proxy_and_preserves_external_changes() {
        use crate::proxy::wide;
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyW};

        struct TestKey(String);
        impl Drop for TestKey {
            fn drop(&mut self) {
                unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide(&self.0).as_ptr()); }
            }
        }
        let key = TestKey(format!(r"Software\SteamBypassTestPAC-{}", std::process::id()));
        let open = || PacSettings { internet_key: &key.0, app_key: &key.0 };
        let current = || config::read_reg_string(&key.0, AUTO_CONFIG).unwrap();
        let original = "http://%PAC_HOST%/proxy.pac";

        assert!(config::write_reg_string(&key.0, AUTO_CONFIG, REG_EXPAND_SZ, original));
        assert!(open().install());
        assert!(open().refresh());
        assert!(is_owned(&current()));
        assert_eq!(open().restore(), Ok(Recovery::Restored));
        assert_eq!(open().restore(), Ok(Recovery::Clean));
        assert_eq!(current(), Some((REG_EXPAND_SZ, original.into())));

        assert!(open().install());
        let replacement = "https://example.test/proxy.pac";
        assert!(config::write_reg_string(&key.0, AUTO_CONFIG, REG_SZ, replacement));
        assert!(!open().refresh());
        assert_eq!(open().restore(), Ok(Recovery::ChangedExternally));
        assert_eq!(current(), Some((REG_SZ, replacement.into())));

        assert!(config::delete_reg_value(&key.0, AUTO_CONFIG));
        assert!(open().install());
        assert_eq!(open().restore(), Ok(Recovery::Restored));
        assert_eq!(current(), None);

        assert!(config::write_reg_string(&key.0, AUTO_CONFIG, REG_SZ, &format!("{}1", pac_prefix())));
        assert_eq!(open().restore(), Ok(Recovery::Restored));
        assert_eq!(current(), None);
    }
}
