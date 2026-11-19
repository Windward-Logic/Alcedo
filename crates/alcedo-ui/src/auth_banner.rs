//! Copy for the sign-in banner above mail.

use alcedo_i18n::{ntr, tr, trf};

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
            SignInStep::Browser => tr!("Sign in"),
            SignInStep::Password => tr!("Update password"),
        };
    }
    let name = shorten(account_name);
    match step {
        SignInStep::Browser => trf!("Sign in to {account}", account = name),
        SignInStep::Password => trf!("Fix {account}", account = name),
    }
}

/// Banner title when accounts are offline but none can be named.
pub(crate) fn unnamed_offline_banner_title(count: usize, keyring_locked: bool) -> String {
    if keyring_locked {
        ntr!(
            "{n} account is offline because the keyring is locked.",
            "{n} accounts are offline because the keyring is locked.",
            count
        )
    } else {
        ntr!(
            "{n} account is offline. Sign in again to reconnect.",
            "{n} accounts are offline. Sign in again to reconnect.",
            count
        )
    }
}

fn shorten(name: &str) -> String {
    if name.chars().count() <= BUTTON_NAME_MAX_CHARS {
        return name.to_string();
    }
    let kept: String = name.chars().take(BUTTON_NAME_MAX_CHARS - 1).collect();
    format!("{kept}…")
}

/// Banner title for mail accounts that cannot sign in.
///
/// One to three names are listed. More than that names the first two and says
/// where to see the rest. Each case is one whole sentence, so every language
/// can phrase it.
pub fn offline_banner_title(names: &[String], keyring_locked: bool) -> String {
    let count = names.len();
    if count == 0 {
        return String::new();
    }
    if count > 3 {
        let (first, second) = first_two(names);
        return if keyring_locked {
            // TRANSLATORS: {n} is four or more; {first} and {second} are account names.
            ntr!(
                "{n} account is offline because the keyring is locked, including {first} and {second}. Open the account menu above the folder list to see the rest. Alcedo will reconnect after you unlock it.",
                "{n} accounts are offline because the keyring is locked, including {first} and {second}. Open the account menu above the folder list to see the rest. Alcedo will reconnect after you unlock it.",
                count,
                first = first,
                second = second
            )
        } else {
            // TRANSLATORS: {n} is four or more; {first} and {second} are account names.
            ntr!(
                "{n} account is offline, including {first} and {second}. Open the account menu above the folder list to see the rest, then sign in again.",
                "{n} accounts are offline, including {first} and {second}. Open the account menu above the folder list to see the rest, then sign in again.",
                count,
                first = first,
                second = second
            )
        };
    }
    let who = join_names(names);
    if keyring_locked {
        // TRANSLATORS: {names} is one to three account names, already joined.
        ntr!(
            "{names} is offline because the keyring is locked. Unlock it and Alcedo will reconnect.",
            "{names} are offline because the keyring is locked. Unlock it and Alcedo will reconnect.",
            count,
            names = who
        )
    } else {
        // TRANSLATORS: {names} is one to three account names, already joined.
        ntr!(
            "{names} is offline. Sign in again to reconnect.",
            "{names} are offline. Sign in again to reconnect.",
            count,
            names = who
        )
    }
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        // TRANSLATORS: two account names in a list.
        [first, second] => trf!("{first} and {second}", first = first, second = second),
        // TRANSLATORS: {list} is two or more names separated by ", "; {last} is the final name.
        [rest @ .., last] => trf!("{list}, and {last}", list = rest.join(", "), last = last),
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
        let title = offline_banner_title(&names(&["Tesla"]), false);
        assert_eq!(title, "Tesla is offline. Sign in again to reconnect.");
    }

    #[test]
    fn two_accounts_are_both_named() {
        let title = offline_banner_title(&names(&["Tesla", "CodeInfusion"]), false);
        assert_eq!(
            title,
            "Tesla and CodeInfusion are offline. Sign in again to reconnect."
        );
    }

    #[test]
    fn many_accounts_name_two_and_point_at_the_menu() {
        let title = offline_banner_title(
            &names(&["Tesla", "CodeInfusion", "Work", "Personal"]),
            false,
        );
        assert_eq!(
            title,
            "4 accounts are offline, including Tesla and CodeInfusion. Open the account menu above the folder list to see the rest, then sign in again."
        );
    }

    #[test]
    fn keyring_lock_says_to_unlock_instead_of_signing_in() {
        let title = offline_banner_title(&names(&["Tesla", "CodeInfusion"]), true);
        assert_eq!(
            title,
            "Tesla and CodeInfusion are offline because the keyring is locked. Unlock it and Alcedo will reconnect."
        );
    }
}
