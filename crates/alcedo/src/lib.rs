//! Alcedo application startup, shared by every binary that runs the app.

use alcedo_core::SyncEvent;
use alcedo_store::StoreHandle;
use alcedo_sync::{SyncExtensions, SyncRuntime};
use alcedo_ui::composer::{
    MailtoPrefill, parse_mailto_uri, set_mailto_launch, take_mailto_open_request,
};
use alcedo_ui::{App, AppInit, present_main_window};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::glib;
use std::rc::Rc;

use alcedo_i18n::{tr, trf};
use alcedo_ui::extension::UiExtension;

/// gettext domain of the public app's text, read by the `alcedo_i18n` macros.
const I18N_DOMAIN: &str = "alcedo";

/// Feature extensions registered with the app at startup.
///
/// The free app runs with none; another binary can depend on this crate and
/// pass its own.
#[derive(Default)]
pub struct Extensions {
    /// UI hooks, registered on the GTK thread before the window is built.
    pub ui: Vec<Rc<dyn UiExtension>>,
    /// Sync-layer hooks, handed to the sync runtime.
    pub sync: SyncExtensions,
}

/// Start the app with `extensions` and block until it quits.
pub fn run(extensions: Extensions) {
    // First, while the process has one thread: it calls setlocale.
    let i18n = alcedo_i18n::init(I18N_DOMAIN);
    init_gsettings_schema_dir();

    // Both aws-lc-rs and ring can end up in the tree via transitive deps; rustls
    // requires an explicit process-level CryptoProvider before any TLS connect.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("install rustls CryptoProvider");

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    if let Err(err) = i18n {
        tracing::warn!(%err, "Alcedo's text stays in English");
    }

    let mock = std::env::var_os("ALCEDO_MOCK").is_some();
    let mailto_from_args = mailto_from_command_line();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio");
    let handle = rt.handle().clone();

    let (ui_event_tx, ui_event_rx) = relm4::channel::<SyncEvent>();
    let (ui_cmd_tx, ui_cmd_rx) = relm4::channel::<alcedo_core::UiCommand>();

    // Mock mode runs on fixture data only: never open (or migrate) the real
    // mail database.
    if !mock {
        let store = match StoreHandle::open_xdg() {
            Ok(store) => store,
            Err(err) => {
                tracing::error!(%err, "could not open the mail store");
                show_startup_error(&err.to_string());
                drop(rt);
                std::process::exit(1);
            }
        };
        let (sync_event_tx, sync_event_rx) = flume::unbounded::<SyncEvent>();
        let (sync_cmd_tx, sync_cmd_rx) = flume::unbounded::<alcedo_core::UiCommand>();

        SyncRuntime::start(
            handle.clone(),
            store.clone(),
            sync_event_tx,
            sync_cmd_rx,
            sync_cmd_tx.clone(),
            extensions.sync,
        );

        handle.spawn(async move {
            while let Ok(event) = sync_event_rx.recv_async().await {
                let _ = ui_event_tx.send(event);
            }
        });

        handle.spawn(async move {
            while let Some(cmd) = ui_cmd_rx.recv().await {
                let _ = sync_cmd_tx.send_async(cmd).await;
            }
        });
    }

    if let Some(prefill) = mailto_from_args {
        set_mailto_launch(prefill);
    }

    for extension in extensions.ui {
        alcedo_ui::extension::register(extension);
    }

    alcedo_ui::theme::install_xdg_app_icon();
    let app = relm4::RelmApp::new("dev.alcedo.Mail");
    alcedo_ui::theme::init();
    install_mailto_handlers();

    app.run::<App>(AppInit {
        mock,
        cmd: ui_cmd_tx,
        events: ui_event_rx,
        runtime: handle,
    });

    drop(rt);
}

/// Show a store-open failure in a dialog instead of panicking, then exit.
fn show_startup_error(details: &str) {
    let body = trf!(
        "Alcedo could not open its mail database, so it cannot start.\n\n{details}\n\nCheck that the data folder (~/.local/share/alcedo, or $ALCEDO_DB) is writable and not used by another Alcedo instance.",
        details = details
    );
    let app = adw::Application::builder()
        .application_id("dev.alcedo.Mail")
        .build();
    app.connect_activate(move |app| {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Alcedo")
            .default_width(480)
            .default_height(200)
            .build();
        window.present();
        let dialog = adw::AlertDialog::builder()
            .heading(tr!("Could not open mail database"))
            .body(&body)
            .build();
        dialog.add_response("quit", &tr!("Quit"));
        dialog.set_default_response(Some("quit"));
        dialog.set_close_response("quit");
        let parent = window.clone();
        dialog.connect_response(None, move |_, _| parent.close());
        dialog.present(Some(&window));
    });
    app.run_with_args::<&str>(&[]);
}

/// Dev-only: the compiled schema dir baked in at build time exists only on the
/// build machine, and a release build's copy may sit in a world-writable
/// directory, so it is used only in debug builds and only when it still holds
/// `gschemas.compiled`. Release builds use the system schema dir.
#[cfg(debug_assertions)]
fn build_schema_dir_usable(dir: &std::path::Path) -> bool {
    dir.join("gschemas.compiled").is_file()
}

#[cfg(not(debug_assertions))]
fn init_gsettings_schema_dir() {}

#[cfg(debug_assertions)]
fn init_gsettings_schema_dir() {
    let build_dir = std::path::Path::new(env!("ALCEDO_SCHEMA_DIR"));
    if std::env::var_os("GSETTINGS_SCHEMA_DIR").is_none() && build_schema_dir_usable(build_dir) {
        // SAFETY: called before any threads are spawned, at the very start of main.
        unsafe {
            std::env::set_var("GSETTINGS_SCHEMA_DIR", build_dir);
        }
    }
}

fn mailto_from_command_line() -> Option<MailtoPrefill> {
    std::env::args()
        .skip(1)
        .find_map(|arg| parse_mailto_uri(&arg))
}

fn install_mailto_handlers() {
    let gtk_app = relm4::main_application();

    // `startup` only runs in the primary instance, so a second launch (for
    // example a mailto: click) never deletes files the running one opened.
    gtk_app.connect_startup(|_app| alcedo_ui::reader::wipe_opened_attachments());

    gtk_app.connect_activate(|_app| {
        present_main_window();
        if take_mailto_open_request() {
            schedule_compose_open();
        }
    });

    gtk_app.connect_open(|_app, files, _hint| {
        for file in files {
            let uri = file.uri().to_string();
            if let Some(prefill) = parse_mailto_uri(&uri) {
                set_mailto_launch(prefill);
                schedule_compose_open();
            }
        }
    });
}

fn schedule_compose_open() {
    glib::idle_add_local_once(|| {
        let gtk_app = relm4::main_application();
        if gtk_app.active_window().is_some() {
            open_composer_from_mailto();
        }
    });
}

fn open_composer_from_mailto() {
    if activate_compose_action() {
        return;
    }
    // The app actions are registered while the main window is built; on the
    // very first activate they may not exist yet, so retry once.
    glib::timeout_add_local_once(std::time::Duration::from_millis(200), || {
        if !activate_compose_action() {
            tracing::warn!("compose action unavailable; mailto launch not opened");
        }
    });
}

/// Open the composer through the `app.compose` action (the same path as the
/// Compose button and Ctrl+N), which applies the pending mailto prefill.
fn activate_compose_action() -> bool {
    let gtk_app = relm4::main_application();
    match gtk_app.lookup_action(alcedo_ui::menu::ACTION_COMPOSE.trim_start_matches("app.")) {
        Some(action) => {
            action.activate(None);
            true
        }
        None => false,
    }
}

#[cfg(all(test, debug_assertions))]
mod schema_dir_tests {
    use super::build_schema_dir_usable;

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("alcedo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn accepts_dir_with_compiled_schemas() {
        let dir = scratch_dir("schema-with");
        std::fs::write(dir.join("gschemas.compiled"), b"").unwrap();
        assert!(build_schema_dir_usable(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_dir_without_compiled_schemas() {
        let dir = scratch_dir("schema-without");
        assert!(!build_schema_dir_usable(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
