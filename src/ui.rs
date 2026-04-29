use std::{
    collections::HashMap,
    iter::FromIterator,
    sync::{Arc, Mutex},
};

use sciter::Value;

use hbb_common::{
    allow_err,
    config::{LocalConfig, PeerConfig},
    log,
};

#[cfg(not(any(feature = "flutter", feature = "cli")))]
use crate::ui_session_interface::Session;
use crate::{common::get_app_name, ipc, ui_interface::*};

mod cm;
#[cfg(feature = "inline")]
pub mod inline;
pub mod remote;

#[allow(dead_code)]
type Status = (i32, bool, i64, String);

lazy_static::lazy_static! {
    // stupid workaround for https://sciter.com/forums/topic/crash-on-latest-tis-mac-sdk-sometimes/
    static ref STUPID_VALUES: Mutex<Vec<Arc<Vec<Value>>>> = Default::default();
}

#[cfg(not(any(feature = "flutter", feature = "cli")))]
lazy_static::lazy_static! {
    pub static ref CUR_SESSION: Arc<Mutex<Option<Session<remote::SciterHandler>>>> = Default::default();
}

struct UIHostHandler;

pub fn start(args: &mut [String]) {
    #[cfg(target_os = "macos")]
    crate::platform::delegate::show_dock();
    #[cfg(all(target_os = "linux", feature = "inline"))]
    {
        let app_dir = std::env::var("APPDIR").unwrap_or("".to_string());
        let mut so_path = "/usr/share/rustdesk/libsciter-gtk.so".to_owned();
        for (prefix, dir) in [
            ("", "/usr"),
            ("", "/app"),
            (&app_dir, "/usr"),
            (&app_dir, "/app"),
        ]
        .iter()
        {
            let path = format!("{prefix}{dir}/share/rustdesk/libsciter-gtk.so");
            if std::path::Path::new(&path).exists() {
                so_path = path;
                break;
            }
        }
        sciter::set_library(&so_path).ok();
    }
    #[cfg(windows)]
    // Check if there is a sciter.dll nearby.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sciter_dll_path = parent.join("sciter.dll");
            if sciter_dll_path.exists() {
                // Try to set the sciter dll.
                let p = sciter_dll_path.to_string_lossy().to_string();
                log::debug!("Found dll:{}, \n {:?}", p, sciter::set_library(&p));
            }
        }
    }
    // https://github.com/c-smile/sciter-sdk/blob/master/include/sciter-x-types.h
    // https://github.com/rustdesk/rustdesk/issues/132#issuecomment-886069737
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::GfxLayer(
        sciter::GFX_LAYER::WARP
    )));
    use sciter::SCRIPT_RUNTIME_FEATURES::*;
    allow_err!(sciter::set_options(sciter::RuntimeOptions::ScriptFeatures(
        ALLOW_FILE_IO as u8 | ALLOW_SOCKET_IO as u8 | ALLOW_EVAL as u8 | ALLOW_SYSINFO as u8
    )));
    let mut frame = sciter::WindowBuilder::main_window().create();
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::UxTheming(true)));
    frame.set_title(&crate::get_app_name());
    #[cfg(target_os = "macos")]
    crate::platform::delegate::make_menubar(frame.get_host(), args.is_empty());
    #[cfg(windows)]
    crate::platform::try_set_window_foreground(frame.get_hwnd() as _);
    let page;
    if args.len() > 1 && args[0] == "--play" {
        args[0] = "--connect".to_owned();
        let path: std::path::PathBuf = (&args[1]).into();
        let id = path
            .file_stem()
            .map(|p| p.to_str().unwrap_or(""))
            .unwrap_or("")
            .to_owned();
        args[1] = id;
    }
    if args.is_empty() {
        std::thread::spawn(move || check_zombie());
        crate::common::check_software_update();
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "index.html";
        // Start pulse audio local server.
        #[cfg(target_os = "linux")]
        std::thread::spawn(crate::ipc::start_pa);
    } else if args[0] == "--install" {
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "install.html";
    } else if args[0] == "--cm" {
        frame.register_behavior("connection-manager", move || {
            Box::new(cm::SciterConnectionManager::new())
        });
        page = "cm.html";
        *cm::HIDE_CM.lock().unwrap() = crate::ipc::get_config("hide_cm")
            .ok()
            .flatten()
            .unwrap_or_default()
            == "true";
    } else if (args[0] == "--connect"
        || args[0] == "--file-transfer"
        || args[0] == "--port-forward"
        || args[0] == "--rdp")
        && args.len() > 1
    {
        #[cfg(windows)]
        {
            let hw = frame.get_host().get_hwnd();
            crate::platform::windows::enable_lowlevel_keyboard(hw as _);
        }
        let mut iter = args.iter();
        let Some(cmd) = iter.next() else {
            log::error!("Failed to get cmd arg");
            return;
        };
        let cmd = cmd.to_owned();
        let Some(id) = iter.next() else {
            log::error!("Failed to get id arg");
            return;
        };
        let id = id.to_owned();
        let pass = iter.next().unwrap_or(&"".to_owned()).clone();
        let args: Vec<String> = iter.map(|x| x.clone()).collect();
        frame.set_title(&id);
        frame.register_behavior("native-remote", move || {
            let handler =
                remote::SciterSession::new(cmd.clone(), id.clone(), pass.clone(), args.clone());
            #[cfg(not(any(feature = "flutter", feature = "cli")))]
            {
                *CUR_SESSION.lock().unwrap() = Some(handler.inner());
            }
            Box::new(handler)
        });
        page = "remote.html";
    } else {
        log::error!("Wrong command: {:?}", args);
        return;
    }
    #[cfg(feature = "inline")]
    {
        let html = if page == "index.html" {
            inline::get_index()
        } else if page == "cm.html" {
            inline::get_cm()
        } else if page == "install.html" {
            inline::get_install()
        } else {
            inline::get_remote()
        };
        frame.load_html(html.as_bytes(), Some(page));
    }
    #[cfg(not(feature = "inline"))]
    frame.load_file(&format!(
        "file://{}/src/ui/{}",
        std::env::current_dir()
            .map(|c| c.display().to_string())
            .unwrap_or("".to_owned()),
        page
    ));
    let hide_cm = *cm::HIDE_CM.lock().unwrap();
    if !args.is_empty() && args[0] == "--cm" && hide_cm {
        // run_app calls expand(show) + run_loop, we use collapse(hide) + run_loop instead to create a hidden window
        frame.collapse(true);
        frame.run_loop();
        return;
    }
    frame.run_app();
}

struct UI {}

impl UI {
    fn recent_sessions_updated(&self) -> bool {
        recent_sessions_updated()
    }

    fn get_id(&self) -> String {
        ipc::get_id()
    }

    fn temporary_password(&mut self) -> String {
        temporary_password()
    }

    fn update_temporary_password(&self) {
        update_temporary_password()
    }

    fn set_permanent_password(&self, password: String) {
        let _ = set_permanent_password_with_result(password);
    }

    fn is_local_permanent_password_set(&self) -> bool {
        is_local_permanent_password_set()
    }

    fn is_permanent_password_set(&self) -> bool {
        is_permanent_password_set()
    }

    fn get_remote_id(&mut self) -> String {
        LocalConfig::get_remote_id()
    }

    fn set_remote_id(&mut self, id: String) {
        LocalConfig::set_remote_id(&id);
    }

    fn goto_install(&mut self) {
        goto_install();
    }

    fn install_me(&mut self, _options: String, _path: String) {
        install_me(_options, _path, false, false);
    }

    fn update_me(&self, _path: String) {
        update_me(_path);
    }

    fn run_without_install(&self) {
        run_without_install();
    }

    fn show_run_without_install(&self) -> bool {
        show_run_without_install()
    }

    fn get_license(&self) -> String {
        get_license()
    }

    fn get_option(&self, key: String) -> String {
        get_option(key)
    }

    fn get_local_option(&self, key: String) -> String {
        get_local_option(key)
    }

    fn set_local_option(&self, key: String, value: String) {
        set_local_option(key, value);
    }

    fn peer_has_password(&self, id: String) -> bool {
        peer_has_password(id)
    }

    fn forget_password(&self, id: String) {
        forget_password(id)
    }

    fn get_peer_option(&self, id: String, name: String) -> String {
        get_peer_option(id, name)
    }

    fn set_peer_option(&self, id: String, name: String, value: String) {
        set_peer_option(id, name, value)
    }

    fn using_public_server(&self) -> bool {
        crate::using_public_server()
    }

    fn is_incoming_only(&self) -> bool {
        hbb_common::config::is_incoming_only()
    }

    pub fn is_outgoing_only(&self) -> bool {
        hbb_common::config::is_outgoing_only()
    }

    pub fn is_custom_client(&self) -> bool {
        crate::common::is_custom_client()
    }

    pub fn is_disable_settings(&self) -> bool {
        hbb_common::config::is_disable_settings()
    }

    pub fn is_disable_account(&self) -> bool {
        hbb_common::config::is_disable_account()
    }

    pub fn is_disable_installation(&self) -> bool {
        hbb_common::config::is_disable_installation()
    }

    pub fn is_disable_ab(&self) -> bool {
        hbb_common::config::is_disable_ab()
    }

    fn get_options(&self) -> Value {
        let hashmap: HashMap<String, String> =
            serde_json::from_str(&get_options()).unwrap_or_default();
        let mut m = Value::map();
        for (k, v) in hashmap {
            m.set_item(k, v);
        }
        m
    }

    fn test_if_valid_server(&self, host: String, test_with_proxy: bool) -> String {
        test_if_valid_server(host, test_with_proxy)
    }

    fn get_sound_inputs(&self) -> Value {
        Value::from_iter(get_sound_inputs())
    }

    fn set_options(&self, v: Value) {
        let mut m = HashMap::new();
        for (k, v) in v.items() {
            if let Some(k) = k.as_string() {
                if let Some(v) = v.as_string() {
                    if !v.is_empty() {
                        m.insert(k, v);
                    }
                }
            }
        }
        set_options(m);
    }

    fn set_option(&self, key: String, value: String) {
        set_option(key, value);
    }

    fn install_path(&mut self) -> String {
        install_path()
    }

    fn install_options(&self) -> String {
        install_options()
    }

    fn get_socks(&self) -> Value {
        Value::from_iter(get_socks())
    }

    fn set_socks(&self, proxy: String, username: String, password: String) {
        set_socks(proxy, username, password)
    }

    fn is_installed(&self) -> bool {
        is_installed()
    }

    fn is_root(&self) -> bool {
        is_root()
    }

    fn is_release(&self) -> bool {
        #[cfg(not(debug_assertions))]
        return true;
        #[cfg(debug_assertions)]
        return false;
    }

    fn is_share_rdp(&self) -> bool {
        is_share_rdp()
    }

    fn set_share_rdp(&self, _enable: bool) {
        set_share_rdp(_enable);
    }

    fn is_installed_lower_version(&self) -> bool {
        is_installed_lower_version()
    }

    fn closing(&mut self, x: i32, y: i32, w: i32, h: i32) {
        crate::server::input_service::fix_key_down_timeout_at_exit();
        LocalConfig::set_size(x, y, w, h);
    }

    fn get_size(&mut self) -> Value {
        let s = LocalConfig::get_size();
        let mut v = Vec::new();
        v.push(s.0);
        v.push(s.1);
        v.push(s.2);
        v.push(s.3);
        Value::from_iter(v)
    }

    fn get_mouse_time(&self) -> f64 {
        get_mouse_time()
    }

    fn check_mouse_time(&self) {
        check_mouse_time()
    }

    fn get_connect_status(&mut self) -> Value {
        let mut v = Value::array(0);
        let x = get_connect_status();
        v.push(x.status_num);
        v.push(x.key_confirmed);
        v.push(x.id);
        v
    }

    #[inline]
    fn get_peer_value(id: String, p: PeerConfig) -> Value {
        let values = vec![
            id,
            p.info.username.clone(),
            p.info.hostname.clone(),
            p.info.platform.clone(),
            p.options.get("alias").unwrap_or(&"".to_owned()).to_owned(),
        ];
        Value::from_iter(values)
    }

    fn get_peer(&self, id: String) -> Value {
        let c = get_peer(id.clone());
        Self::get_peer_value(id, c)
    }

    fn get_fav(&self) -> Value {
        Value::from_iter(get_fav())
    }

    fn store_fav(&self, fav: Value) {
        let mut tmp = vec![];
        fav.values().for_each(|v| {
            if let Some(v) = v.as_string() {
                if !v.is_empty() {
                    tmp.push(v);
                }
            }
        });
        store_fav(tmp);
    }

    fn get_recent_sessions(&mut self) -> Value {
        // to-do: limit number of recent sessions, and remove old peer file
        let peers: Vec<Value> = PeerConfig::peers(None)
            .drain(..)
            .map(|p| Self::get_peer_value(p.0, p.2))
            .collect();
        Value::from_iter(peers)
    }

    fn get_icon(&mut self) -> String {
        get_icon()
    }

    fn remove_peer(&mut self, id: String) {
        PeerConfig::remove(&id);
    }

    fn remove_discovered(&mut self, id: String) {
        remove_discovered(id);
    }

    fn send_wol(&mut self, id: String) {
        crate::lan::send_wol(id)
    }

    fn new_remote(&mut self, id: String, remote_type: String, force_relay: bool) {
        new_remote(id, remote_type, force_relay)
    }

    fn is_process_trusted(&mut self, _prompt: bool) -> bool {
        is_process_trusted(_prompt)
    }

    fn is_can_screen_recording(&mut self, _prompt: bool) -> bool {
        is_can_screen_recording(_prompt)
    }

    fn is_installed_daemon(&mut self, _prompt: bool) -> bool {
        is_installed_daemon(_prompt)
    }

    fn get_error(&mut self) -> String {
        get_error()
    }

    fn is_login_wayland(&mut self) -> bool {
        is_login_wayland()
    }

    fn current_is_wayland(&mut self) -> bool {
        current_is_wayland()
    }

    fn get_software_update_url(&self) -> String {
        crate::SOFTWARE_UPDATE_URL.lock().unwrap().clone()
    }

    fn get_new_version(&self) -> String {
        get_new_version()
    }

    fn get_version(&self) -> String {
        get_version()
    }

    fn get_fingerprint(&self) -> String {
        get_fingerprint()
    }

    fn get_app_name(&self) -> String {
        get_app_name()
    }

    fn get_software_ext(&self) -> String {
        #[cfg(windows)]
        let p = "exe";
        #[cfg(target_os = "macos")]
        let p = "dmg";
        #[cfg(target_os = "linux")]
        let p = "deb";
        p.to_owned()
    }

    fn get_software_store_path(&self) -> String {
        let mut p = std::env::temp_dir();
        let name = crate::SOFTWARE_UPDATE_URL
            .lock()
            .unwrap()
            .split("/")
            .last()
            .map(|x| x.to_owned())
            .unwrap_or(crate::get_app_name());
        p.push(name);
        format!("{}.{}", p.to_string_lossy(), self.get_software_ext())
    }

    fn create_shortcut(&self, _id: String) {
        #[cfg(windows)]
        create_shortcut(_id)
    }

    fn discover(&self) {
        std::thread::spawn(move || {
            allow_err!(crate::lan::discover());
        });
    }

    fn get_lan_peers(&self) -> String {
        // let peers = get_lan_peers()
        //     .into_iter()
        //     .map(|mut peer| {
        //         (
        //             peer.remove("id").unwrap_or_default(),
        //             peer.remove("username").unwrap_or_default(),
        //             peer.remove("hostname").unwrap_or_default(),
        //             peer.remove("platform").unwrap_or_default(),
        //         )
        //     })
        //     .collect::<Vec<(String, String, String, String)>>();
        serde_json::to_string(&get_lan_peers()).unwrap_or_default()
    }

    fn get_uuid(&self) -> String {
        get_uuid()
    }

    fn open_url(&self, url: String) {
        #[cfg(windows)]
        let p = "explorer";
        #[cfg(target_os = "macos")]
        let p = "open";
        #[cfg(target_os = "linux")]
        let p = if std::path::Path::new("/usr/bin/firefox").exists() {
            "firefox"
        } else {
            "xdg-open"
        };
        allow_err!(std::process::Command::new(p).arg(url).spawn());
    }

    fn change_id(&self, id: String) {
        reset_async_job_status();
        let old_id = self.get_id();
        change_id_shared(id, old_id);
    }

    fn http_request(&self, url: String, method: String, body: Option<String>, header: String) {
        http_request(url, method, body, header)
    }

    fn post_request(&self, url: String, body: String, header: String) {
        post_request(url, body, header)
    }

    fn is_ok_change_id(&self) -> bool {
        hbb_common::machine_uid::get().is_ok()
    }

    fn get_async_job_status(&self) -> String {
        get_async_job_status()
    }

    fn get_http_status(&self, url: String) -> Option<String> {
        get_async_http_status(url)
    }

    fn t(&self, name: String) -> String {
        crate::client::translate(name)
    }

    fn is_xfce(&self) -> bool {
        crate::platform::is_xfce()
    }

    fn get_api_server(&self) -> String {
        get_api_server()
    }

    fn has_hwcodec(&self) -> bool {
        has_hwcodec()
    }

    fn has_vram(&self) -> bool {
        has_vram()
    }

    fn get_langs(&self) -> String {
        get_langs()
    }

    fn video_save_directory(&self, root: bool) -> String {
        video_save_directory(root)
    }

    fn handle_relay_id(&self, id: String) -> String {
        handle_relay_id(&id).to_owned()
    }

    fn get_login_device_info(&self) -> String {
        get_login_device_info_json()
    }

    fn support_remove_wallpaper(&self) -> bool {
        support_remove_wallpaper()
    }

    fn has_valid_2fa(&self) -> bool {
        has_valid_2fa()
    }

    fn generate2fa(&self) -> String {
        generate2fa()
    }

    pub fn verify2fa(&self, code: String) -> bool {
        verify2fa(code)
    }

    fn verify_login(&self, raw: String, id: String) -> bool {
        crate::verify_login(&raw, &id)
    }

    fn generate_2fa_img_src(&self, data: String) -> String {
        let v = qrcode_generator::to_png_to_vec(data, qrcode_generator::QrCodeEcc::Low, 128)
            .unwrap_or_default();
        let s = hbb_common::sodiumoxide::base64::encode(
            v,
            hbb_common::sodiumoxide::base64::Variant::Original,
        );
        format!("data:image/png;base64,{s}")
    }

    pub fn check_hwcodec(&self) {
        check_hwcodec()
    }

    fn is_option_fixed(&self, key: String) -> bool {
        crate::ui_interface::is_option_fixed(&key)
    }

    fn get_builtin_option(&self, key: String) -> String {
        crate::ui_interface::get_builtin_option(&key)
    }

    fn is_remote_modify_enabled_by_control_permissions(&self) -> String {
        match crate::ui_interface::is_remote_modify_enabled_by_control_permissions() {
            Some(true) => "true",
            Some(false) => "false",
            None => "",
        }
        .to_string()
    }
}

impl sciter::EventHandler for UI {
    sciter::dispatch_script_call! {
        fn t(String);
        fn get_api_server();
        fn is_xfce();
        fn using_public_server();
        fn is_custom_client();
        fn is_outgoing_only();
        fn is_incoming_only();
        fn is_disable_settings();
        fn is_disable_account();
        fn is_disable_installation();
        fn is_disable_ab();
        fn get_id();
        fn temporary_password();
        fn update_temporary_password();
        fn set_permanent_password(String);
        fn is_local_permanent_password_set();
        fn is_permanent_password_set();
        fn get_remote_id();
        fn set_remote_id(String);
        fn closing(i32, i32, i32, i32);
        fn get_size();
        fn new_remote(String, String, bool);
        fn send_wol(String);
        fn remove_peer(String);
        fn remove_discovered(String);
        fn get_connect_status();
        fn get_mouse_time();
        fn check_mouse_time();
        fn get_recent_sessions();
        fn get_peer(String);
        fn get_fav();
        fn store_fav(Value);
        fn recent_sessions_updated();
        fn get_icon();
        fn install_me(String, String);
        fn is_installed();
        fn is_root();
        fn is_release();
        fn set_socks(String, String, String);
        fn get_socks();
        fn is_share_rdp();
        fn set_share_rdp(bool);
        fn is_installed_lower_version();
        fn install_path();
        fn install_options();
        fn goto_install();
        fn is_process_trusted(bool);
        fn is_can_screen_recording(bool);
        fn is_installed_daemon(bool);
        fn get_error();
        fn is_login_wayland();
        fn current_is_wayland();
        fn get_options();
        fn get_option(String);
        fn get_local_option(String);
        fn set_local_option(String, String);
        fn get_peer_option(String, String);
        fn peer_has_password(String);
        fn forget_password(String);
        fn set_peer_option(String, String, String);
        fn get_license();
        fn test_if_valid_server(String, bool);
        fn get_sound_inputs();
        fn set_options(Value);
        fn set_option(String, String);
        fn get_software_update_url();
        fn get_new_version();
        fn get_version();
        fn get_fingerprint();
        fn update_me(String);
        fn show_run_without_install();
        fn run_without_install();
        fn get_app_name();
        fn get_software_store_path();
        fn get_software_ext();
        fn open_url(String);
        fn change_id(String);
        fn get_async_job_status();
        fn post_request(String, String, String);
        fn is_ok_change_id();
        fn create_shortcut(String);
        fn discover();
        fn get_lan_peers();
        fn get_uuid();
        fn has_hwcodec();
        fn has_vram();
        fn get_langs();
        fn video_save_directory(bool);
        fn handle_relay_id(String);
        fn get_login_device_info();
        fn support_remove_wallpaper();
        fn has_valid_2fa();
        fn generate2fa();
        fn generate_2fa_img_src(String);
        fn verify2fa(String);
        fn check_hwcodec();
        fn verify_login(String, String);
        fn is_option_fixed(String);
        fn get_builtin_option(String);
        fn is_remote_modify_enabled_by_control_permissions();
    }
}

impl sciter::host::HostHandler for UIHostHandler {
    fn on_graphics_critical_failure(&mut self) {
        log::error!("Critical rendering error: e.g. DirectX gfx driver error. Most probably bad gfx drivers.");
    }
}

#[cfg(not(target_os = "linux"))]
fn get_sound_inputs() -> Vec<String> {
    let mut out = Vec::new();
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    if let Ok(devices) = host.devices() {
        for device in devices {
            if device.default_input_config().is_err() {
                continue;
            }
            if let Ok(name) = device.name() {
                out.push(name);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn get_sound_inputs() -> Vec<String> {
    crate::platform::linux::get_pa_sources()
        .drain(..)
        .map(|x| x.1)
        .collect()
}

// sacrifice some memory
pub fn value_crash_workaround(values: &[Value]) -> Arc<Vec<Value>> {
    let persist = Arc::new(values.to_vec());
    STUPID_VALUES.lock().unwrap().push(persist.clone());
    persist
}

pub fn get_icon() -> String {
    // 128x128
    #[cfg(target_os = "macos")]
    // 128x128 on 160x160 canvas, then shrink to 128, mac looks better with padding
    {
        "data:image/png;base64,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".into()
    }
    #[cfg(not(target_os = "macos"))] // 128x128 no padding
    {
        "data:image/png;base64,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".into()
    }
}
