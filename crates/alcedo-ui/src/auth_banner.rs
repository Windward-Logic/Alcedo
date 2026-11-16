//! Copy for the sign-in banner above mail.

/// Shown for four or more offline mail accounts, which the title cannot all name.
pub(crate) const MAIL_SIGN_IN_LIST_HINT: &str =
    "Open the account menu above the folder list to see the rest, then sign in again.";

/// Shown for four or more mail accounts offline because the keyring is locked.
pub(crate) const MAIL_KEYRING_LIST_HINT: &str =
    "Open the account menu above the folder list to see the rest.";

/// What one offline account asks of the person before it can reconnect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignInStep {
    /// Approve access again in the browser (Google, Microsoft).
    Browser,
    /// Type the account password again in the account editor.
    Password,
}

const BUTTON_NAME_MAX_CHARS: usize = 14;

/// Banner button text that says what pressing it does.
///
/// With one account offline the verb is enough. With several, the button acts
/// on one of them, so it names that account.
pub(crate) fn sign_in_button_label(
    step: SignInStep,
    offline_count: usize,
    account_name: &str,
) -> String {
    if offline_count <= 1 {
        return match step {
            SignInStep::Browser => "Sign in".to_string(),
            SignInStep::Password => "Update password".to_string(),
        };
    }
    let name = shorten(account_name);
    match step {
        SignInStep::Browser => format!("Sign in to {name}"),
        SignInStep::Password => format!("Fix {name}"),
    }
}

/// Banner title when accounts are offline but none can be named.
pub(crate) fn unnamed_offline_banner_title(count: usize, keyring_locked: bool) -> String {
    if keyring_locked {
        format!("{count} accounts are offline because the keyring is locked.")
    } else {
        format!("{count} accounts are offline. Sign in again to reconnect.")
    }
}

fn shorten(name: &str) -> String {
    if name.chars().count() <= BUTTON_NAME_MAX_CHARS {
        return name.to_string();
    }
    let kept: String = name.chars().take(BUTTON_NAME_MAX_CHARS - 1).collect();
    format!("{kept}…")
}

/// Banner title for accounts that cannot sign in.
///
/// One to three names are listed. More than that names the first two and
/// `list_hint` tells the person where to see the rest.
pub fn offline_banner_title(
    names: &[String],
    noun: &str,
    keyring_locked: bool,
    list_hint: &str,
) -> String {
    let count = names.len();
    let who = join_names(names);
    if keyring_locked {
        match count {
            0 => String::new(),
            1 => format!(
                "{who} is offline because the keyring is locked. Unlock it and Alcedo will reconnect."
            ),
            2 | 3 => format!(
                "{who} are offline because the keyring is locked. Unlock it and Alcedo will reconnect."
            ),
            _ => {
                let (first, second) = first_two(names);
                format!(
                    "{count} {noun}s are offline because the keyring is locked, including {first} and {second}. {list_hint} Alcedo will reconnect after you unlock it."
                )
            }
        }
    } else {
        match count {
            0 => String::new(),
            1 => format!("{who} is offline. Sign in again to reconnect."),
            2 | 3 => format!("{who} are offline. Sign in again to reconnect."),
            _ => {
                let (first, second) = first_two(names);
                format!("{count} {noun}s are offline, including {first} and {second}. {list_hint}")
            }
        }
    }
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let (last, rest) = names.split_last().unwrap();
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

fn first_two(names: &[String]) -> (&str, &str) {
    (
        names.first().map(String::as_str).unwrap_or(""),
        names.get(1).map(String::as_str).unwrap_or(""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    const MAIL_HINT: &str = MAIL_SIGN_IN_LIST_HINT;

    #[test]
    fn single_offline_account_button_is_the_bare_verb() {
        assert_eq!(
            sign_in_button_label(SignInStep::Browser, 1, "Gmail"),
            "Sign in"
        );
        assert_eq!(
            sign_in_button_label(SignInStep::Password, 1, "Gmail"),
            "Update password"
        );
    }

    #[test]
    fn several_offline_accounts_button_names_the_target() {
        assert_eq!(
            sign_in_button_label(SignInStep::Browser, 2, "Gmail"),
            "Sign in to Gmail"
        );
        assert_eq!(
            sign_in_button_label(SignInStep::Password, 3, "Work"),
            "Fix Work"
        );
    }

    #[test]
    fn long_account_names_are_shortened_in_the_button() {
        assert_eq!(
            sign_in_button_label(SignInStep::Browser, 2, "Windward Logic Consulting"),
            "Sign in to Windward Logi…"
        );
    }

    #[test]
    fn unnamed_title_still_says_why_the_accounts_are_offline() {
        assert_eq!(
            unnamed_offline_banner_title(2, true),
            "2 accounts are offline because the keyring is locked."
        );
        assert_eq!(
            unnamed_offline_banner_title(2, false),
            "2 accounts are offline. Sign in again to reconnect."
        );
    }

    #[test]
    fn one_account_is_named() {
        let title = offline_banner_title(&names(&["Tesla"]), "account", false, MAIL_HINT);
        assert_eq!(title, "Tesla is offline. Sign in again to reconnect.");
    }

    #[test]
    fn two_accounts_are_both_named() {
        let title = offline_banner_title(
            &names(&["Tesla", "CodeInfusion"]),
            "account",
            false,
            MAIL_HINT,
        );
        assert_eq!(
            title,
            "Tesla and CodeInfusion are offline. Sign in again to reconnect."
        );
    }

    #[test]
    fn many_accounts_name_two_and_point_at_the_menu() {
        let title = offline_banner_title(
            &names(&["Tesla", "CodeInfusion", "Work", "Personal"]),
            "account",
            false,
            MAIL_HINT,
        );
        assert_eq!(
            title,
            "4 accounts are offline, including Tesla and CodeInfusion. Open the account menu above the folder list to see the rest, then sign in again."
        );
    }

    #[test]
    fn keyring_lock_says_to_unlock_instead_of_signing_in() {
        let title = offline_banner_title(
            &names(&["Tesla", "CodeInfusion"]),
            "account",
            true,
            MAIL_HINT,
        );
        assert_eq!(
            title,
            "Tesla and CodeInfusion are offline because the keyring is locked. Unlock it and Alcedo will reconnect."
        );
    }
}
