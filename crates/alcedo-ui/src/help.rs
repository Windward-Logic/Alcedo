//! In-app help window backed by bundled HTML with section anchors.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;

use alcedo_widgets::{create_isolated_context, ephemeral_network_session};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use tokio::runtime::Handle;
use webkit6::prelude::*;
use webkit6::{LoadEvent, NavigationPolicyDecision, PolicyDecisionType, WebView};

use crate::extension::{self, HelpWindowContext};

static BUNDLED_HELP_HTML: &str = include_str!("../../../data/resources/help/index.html");

struct HelpHost {
    window: adw::Window,
    web_view: WebView,
    loaded: Rc<Cell<bool>>,
    pending: Rc<RefCell<Option<String>>>,
    section: Rc<RefCell<String>>,
}

/// GResource path for the help document.
pub const HELP_RESOURCE: &str = "/dev/alcedo/Mail/help/index.html";

pub const TOPIC_OVERVIEW: &str = "overview";
pub const TOPIC_LAYOUT: &str = "layout";
pub const TOPIC_ACCOUNTS: &str = "accounts";
pub const TOPIC_FOLDERS: &str = "folders";
pub const TOPIC_SEARCH: &str = "search";
pub const TOPIC_READING: &str = "reading";
pub const TOPIC_CONVERSATIONS: &str = "conversations";
pub const TOPIC_COMPOSE: &str = "compose";
pub const TOPIC_IDENTITIES: &str = "identities";
pub const TOPIC_SNIPPETS: &str = "snippets";
pub const TOPIC_OPENPGP: &str = "openpgp";
pub const TOPIC_RULES: &str = "rules";
pub const TOPIC_CONTACTS: &str = "contacts";
pub const TOPIC_PRIVACY: &str = "privacy";
pub const TOPIC_NOTIFICATIONS: &str = "notifications";
pub const TOPIC_APPEARANCE: &str = "appearance";
pub const TOPIC_KEYBOARD: &str = "keyboard";
pub const TOPIC_SHORTCUTS: &str = "shortcuts";

/// Topics that must exist as `id` attributes in the help HTML.
pub const TOPICS: &[&str] = &[
    TOPIC_OVERVIEW,
    TOPIC_LAYOUT,
    TOPIC_ACCOUNTS,
    TOPIC_FOLDERS,
    TOPIC_SEARCH,
    TOPIC_READING,
    TOPIC_CONVERSATIONS,
    TOPIC_COMPOSE,
    TOPIC_IDENTITIES,
    TOPIC_SNIPPETS,
    TOPIC_OPENPGP,
    TOPIC_RULES,
    TOPIC_CONTACTS,
    TOPIC_PRIVACY,
    TOPIC_NOTIFICATIONS,
    TOPIC_APPEARANCE,
    TOPIC_KEYBOARD,
    TOPIC_SHORTCUTS,
];

use crate::menu::ACTION_HELP_TOPIC;

thread_local! {
    static HOST: RefCell<Option<HelpHost>> = const { RefCell::new(None) };
}

/// Open or focus help, scrolling to `topic` when it is a known section id.
pub fn present(parent: &impl IsA<gtk::Window>, topic: Option<&str>, runtime: Handle) {
    let parent = parent.as_ref();
    let topic = topic.and_then(|raw| valid_topic(raw).then(|| raw.to_string()));
    let section = topic.clone().unwrap_or_else(|| TOPIC_OVERVIEW.to_string());

    HOST.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(build_host(parent, runtime.clone()));
        }
        let host = slot.borrow();
        let Some(host) = host.as_ref() else {
            return;
        };
        host.window.set_transient_for(Some(parent));
        *host.section.borrow_mut() = section;
        if let Some(id) = topic {
            *host.pending.borrow_mut() = Some(id);
            if host.loaded.get() {
                scroll_pending(&host.web_view, &host.pending);
            }
        }
        apply_help_theme(&host.web_view);
        host.window.present();
    });
}

pub fn help_guide_text(html: &str) -> String {
    let mut s = html.to_string();
    if let Some(start) = s.find("<main>")
        && let Some(end) = s.find("</main>")
    {
        let end = end + "</main>".len();
        s = s[start..end].to_string();
    }

    loop {
        let lower = s.to_lowercase();
        let Some(start) = lower.find("<script") else {
            break;
        };
        let Some(rel) = lower[start..].find("</script>") else {
            break;
        };
        let end = start + rel + "</script>".len();
        s.replace_range(start..end, "");
    }

    for tag in ["</h2>", "</h3>", "</p>", "</li>", "</tr>", "</section>"] {
        s = s.replace(tag, "\n");
    }
    s = s.replace("</td>", " | ");
    s = s.replace("</th>", " | ");
    for br in ["<br />", "<br/>", "<br>", "<BR />", "<BR/>", "<BR>"] {
        s = s.replace(br, "\n");
    }

    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            while chars.next().is_some_and(|c| c != '>') {}
            continue;
        }
        out.push(ch);
    }

    let mut decoded = out;
    for (entity, replacement) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ] {
        decoded = decoded.replace(entity, replacement);
    }

    let mut lines: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.push(collapsed);
    }

    let mut trimmed_lines: Vec<String> = Vec::new();
    let mut blank_run = false;
    for line in lines {
        if line.is_empty() {
            if !blank_run {
                trimmed_lines.push(String::new());
                blank_run = true;
            }
        } else {
            trimmed_lines.push(line);
            blank_run = false;
        }
    }
    while trimmed_lines.first().is_some_and(String::is_empty) {
        trimmed_lines.remove(0);
    }
    while trimmed_lines.last().is_some_and(String::is_empty) {
        trimmed_lines.pop();
    }
    trimmed_lines.join("\n")
}

static GUIDE_TEXT: OnceLock<String> = OnceLock::new();

/// The help guide as plain text, including extension sections.
pub fn guide_text() -> String {
    GUIDE_TEXT
        .get_or_init(|| help_guide_text(&help_html(BUNDLED_HELP_HTML)))
        .clone()
}

/// The bundled guide with every registered extension's sections merged in.
fn help_html(bundled: &str) -> String {
    let sections: Vec<_> = extension::registered()
        .iter()
        .flat_map(|ext| ext.help_sections())
        .collect();
    extension::merge_help_sections(bundled, &sections)
}

/// Section ids added by extensions; valid topics alongside `TOPICS`.
fn extension_topics() -> Vec<&'static str> {
    extension::registered()
        .iter()
        .flat_map(|ext| ext.help_sections())
        .map(|section| section.id)
        .collect()
}

/// Flat circular ? button that opens help at `topic`.
pub fn help_button(topic: &'static str) -> gtk::Button {
    debug_assert!(
        valid_topic(topic),
        "unknown help topic {topic}; add it to TOPICS and the HTML"
    );
    let btn = gtk::Button::builder()
        .icon_name("dialog-question-symbolic")
        .tooltip_text("Help")
        .css_classes(["flat", "circular", "alcedo-help-btn"])
        .valign(gtk::Align::Center)
        .focus_on_click(false)
        .action_name(ACTION_HELP_TOPIC)
        .build();
    btn.set_action_target_value(Some(&glib::Variant::from(topic)));
    btn.update_property(&[gtk::accessible::Property::Label("Help")]);
    btn
}

/// Attach a ? button to a preferences group header.
pub fn attach_group_help(group: &adw::PreferencesGroup, topic: &'static str) {
    group.set_header_suffix(Some(&help_button(topic)));
}

fn valid_topic(topic: &str) -> bool {
    TOPICS.contains(&topic) || extension_topics().contains(&topic)
}

fn build_host(parent: &gtk::Window, runtime: Handle) -> HelpHost {
    let window = adw::Window::builder()
        .title("Alcedo Help")
        .default_width(900)
        .default_height(640)
        .hide_on_close(true)
        .transient_for(parent)
        .build();
    if let Some(app) = parent.application() {
        window.set_application(Some(&app));
    }
    window.connect_map(|window| {
        crate::theme::apply_toplevel_icon(window);
    });

    let loaded = Rc::new(Cell::new(false));
    let pending = Rc::new(RefCell::new(Some(TOPIC_OVERVIEW.to_string())));
    let section = Rc::new(RefCell::new(TOPIC_OVERVIEW.to_string()));

    let web_view = build_help_web_view();
    wire_help_navigation_policy(&web_view);
    load_help_html(&web_view);

    web_view.connect_load_changed({
        let loaded = loaded.clone();
        let pending = pending.clone();
        move |view, event| {
            if event != LoadEvent::Finished {
                return;
            }
            loaded.set(true);
            apply_help_theme(view);
            scroll_pending(view, &pending);
        }
    });

    adw::StyleManager::default().connect_notify_local(Some("dark"), {
        let web_view = web_view.clone();
        move |_, _| apply_help_theme(&web_view)
    });

    let header = adw::HeaderBar::new();
    let ctx = HelpWindowContext {
        window: window.clone(),
        runtime,
        current_section: {
            let section = section.clone();
            Rc::new(move || section.borrow().clone())
        },
    };
    for ext in extension::registered() {
        for widget in ext.help_header_widgets(&ctx) {
            header.pack_end(&widget);
        }
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&web_view));
    window.set_content(Some(&toolbar));

    HelpHost {
        window: window.clone(),
        web_view,
        loaded,
        pending,
        section,
    }
}

fn build_help_web_view() -> WebView {
    let settings = webkit6::Settings::default();
    settings.set_enable_javascript(true);
    settings.set_auto_load_images(false);
    settings.set_javascript_can_access_clipboard(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_enable_write_console_messages_to_stdout(false);

    let web_view = WebView::builder()
        .web_context(&create_isolated_context())
        .network_session(&ephemeral_network_session())
        .settings(&settings)
        .default_content_security_policy(
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src 'none'; connect-src 'none'; frame-src 'none'",
        )
        .hexpand(true)
        .vexpand(true)
        .css_classes(["alcedo-help-view"])
        .build();
    web_view.set_size_request(1, 1);
    web_view.update_property(&[gtk::accessible::Property::Label("Help")]);
    web_view
}

fn load_help_html(web_view: &WebView) {
    match gio::resources_lookup_data(HELP_RESOURCE, gio::ResourceLookupFlags::NONE) {
        Ok(bytes) => {
            let html = help_html(&String::from_utf8_lossy(bytes.as_ref()));
            web_view.load_html(&html, Some("about:blank"));
        }
        Err(error) => {
            tracing::warn!(?error, "help HTML missing from gresource");
            web_view.load_html("<p>Help could not be loaded.</p>", Some("about:blank"));
        }
    }
}

fn wire_help_navigation_policy(web_view: &WebView) {
    web_view.connect_decide_policy(move |_view, decision, decision_type| {
        let uri = match decision_type {
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
                let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else {
                    return false;
                };
                let Some(action) = nav.navigation_action() else {
                    return false;
                };
                let Some(request) = action.request() else {
                    return false;
                };
                request.uri().map(|uri| uri.to_string())
            }
            _ => return false,
        };
        let Some(uri) = uri else {
            return false;
        };
        if uri.starts_with("about:blank") || uri == "about:blank" {
            return false;
        }
        decision.ignore();
        true
    });
}

fn apply_help_theme(web_view: &WebView) {
    let dark = adw::StyleManager::default().is_dark();
    let script = if dark {
        "document.documentElement.classList.add('dark');"
    } else {
        "document.documentElement.classList.remove('dark');"
    };
    web_view.evaluate_javascript(script, None, None, None::<&gio::Cancellable>, |_| {});
}

fn scroll_pending(web_view: &WebView, pending: &Rc<RefCell<Option<String>>>) {
    let Some(id) = pending.borrow_mut().take() else {
        return;
    };
    if !valid_topic(&id) {
        return;
    }
    let script = format!(
        "(function() {{
            var id = {id:?};
            var el = document.getElementById(id);
            if (el) {{ el.scrollIntoView({{ block: 'start' }}); }}
            if (history.replaceState) {{
              history.replaceState(null, '', '#' + id);
            }}
            var links = document.querySelectorAll('nav a');
            links.forEach(function(a) {{
              a.classList.toggle('active', a.getAttribute('href') === '#' + id);
            }});
        }})();"
    );
    web_view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP_HTML: &str = include_str!("../../../data/resources/help/index.html");

    #[test]
    fn topics_are_valid_slugs() {
        for topic in TOPICS {
            assert!(
                topic
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
                "topic {topic} is not a slug"
            );
            assert!(valid_topic(topic));
        }
    }

    #[test]
    fn help_html_declares_every_topic_id() {
        for topic in TOPICS {
            let needle = format!("id=\"{topic}\"");
            assert!(HELP_HTML.contains(&needle), "help HTML missing {needle}");
            let href = format!("href=\"#{topic}\"");
            assert!(HELP_HTML.contains(&href), "help TOC missing {href}");
        }
    }

    #[test]
    fn help_guide_text_extracts_prose() {
        let text = help_guide_text(HELP_HTML);
        assert!(text.contains("One is the From address that people see."));
        assert!(text.contains("Sender"));
        assert!(text.contains("Catch-all domains"));
        assert!(!text.contains("<p"));
        assert!(!text.contains("function mark"));
    }

    #[test]
    fn unknown_topic_is_rejected() {
        assert!(!valid_topic(""));
        assert!(!valid_topic("overview;alert(1)"));
        assert!(!valid_topic("../etc"));
    }
}
