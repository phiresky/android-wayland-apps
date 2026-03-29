//! XDG Desktop Portal bridge.
//!
//! Listens on a Unix socket inside the Arch rootfs. A Python portal backend
//! (xdg-desktop-portal-android) connects and sends JSON requests when a Linux
//! app opens a file dialog. This module forwards the request to Android via JNI,
//! launching a FileChooserActivity, and returns the result back over the socket.

use crate::android::utils::jni_context;
use crate::android::utils::socket::create_unix_listener;
use crate::android::window_manager::{send_event, WindowEvent};
use crate::core::config;

use jni::objects::JValue;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Mutex;

/// Pending portal requests awaiting a response from Android.
static PENDING: Mutex<Option<HashMap<String, mpsc::Sender<PortalResponse>>>> = Mutex::new(None);

/// A portal request from the Python backend.
#[derive(Debug, Clone)]
pub struct PortalRequest {
    pub id: String,
    pub request_type: PortalRequestType,
    pub title: String,
    pub multiple: bool,
    pub directory: bool,
    pub mime_types: Vec<String>,
    pub current_name: Option<String>,
    /// Channel to send the response back to the socket handler thread.
    pub response_tx: mpsc::Sender<PortalResponse>,
}

#[derive(Debug, Clone)]
pub enum PortalRequestType {
    OpenFile,
    SaveFile,
}

/// Response from Android back to the portal backend.
#[derive(Debug, Clone)]
pub struct PortalResponse {
    pub id: String,
    /// 0 = success, 1 = cancelled, 2 = error
    pub response: u32,
    pub uris: Vec<String>,
}

/// Start the portal bridge listener on a background thread.
/// Called after setup completes and the compositor is ready.
pub fn start_portal_bridge() {
    std::thread::spawn(|| {
        if let Err(e) = run_listener() {
            tracing::error!("Portal bridge listener failed: {e}");
        }
    });
}

fn run_listener() -> std::io::Result<()> {
    // Initialize the pending requests map.
    if let Ok(mut guard) = PENDING.lock() {
        *guard = Some(HashMap::new());
    }

    let socket_path = format!("{}/tmp/.portal-bridge", config::ARCH_FS_ROOT);
    let listener = create_unix_listener(Path::new(&socket_path))?;
    tracing::info!("Portal bridge listening on {socket_path}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(e) = handle_client(stream) {
                        tracing::warn!("Portal client disconnected: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("Portal bridge accept failed: {e}");
            }
        }
    }

    Ok(())
}

fn handle_client(stream: UnixStream) -> std::io::Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        tracing::debug!("Portal request: {line}");

        // Parse the JSON request.
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Invalid portal JSON: {e}");
                let err = serde_json::json!({"error": "invalid json"});
                writeln!(writer, "{err}")?;
                continue;
            }
        };

        let id = req["id"].as_str().unwrap_or("0").to_string();

        // Handle settings queries synchronously (no compositor round-trip needed).
        if req["type"].as_str() == Some("get_color_scheme") {
            let scheme = get_android_color_scheme();
            let resp = serde_json::json!({"color_scheme": scheme});
            writeln!(writer, "{resp}")?;
            writer.flush()?;
            continue;
        }

        let request_type = match req["type"].as_str().unwrap_or("") {
            "open_file" => PortalRequestType::OpenFile,
            "save_file" => PortalRequestType::SaveFile,
            other => {
                tracing::error!("Unknown portal request type: {other}");
                let resp = serde_json::json!({"id": id, "response": 2, "uris": []});
                writeln!(writer, "{resp}")?;
                continue;
            }
        };

        let title = req["title"].as_str().unwrap_or("Choose a file").to_string();
        let multiple = req["multiple"].as_bool().unwrap_or(false);
        let directory = req["directory"].as_bool().unwrap_or(false);
        let mime_types: Vec<String> = req["mime_types"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let current_name = req["current_name"].as_str().map(String::from);

        // Create a oneshot-like channel for the response.
        let (tx, rx) = mpsc::channel();

        // Register the pending request.
        if let Ok(mut guard) = PENDING.lock() {
            if let Some(map) = guard.as_mut() {
                map.insert(id.clone(), tx.clone());
            }
        }

        // Send the request to the compositor thread via WindowEvent.
        send_event(WindowEvent::PortalRequest(PortalRequest {
            id: id.clone(),
            request_type,
            title,
            multiple,
            directory,
            mime_types,
            current_name,
            response_tx: tx,
        }));

        // Block waiting for Android to respond.
        let response = match rx.recv() {
            Ok(resp) => resp,
            Err(_) => PortalResponse {
                id: id.clone(),
                response: 2,
                uris: vec![],
            },
        };

        // Clean up pending map.
        if let Ok(mut guard) = PENDING.lock() {
            if let Some(map) = guard.as_mut() {
                map.remove(&id);
            }
        }

        // Send JSON response back to the portal backend.
        let resp_json = serde_json::json!({
            "id": response.id,
            "response": response.response,
            "uris": response.uris,
        });
        writeln!(writer, "{resp_json}")?;
        writer.flush()?;
    }

    Ok(())
}

/// Read Android's current color scheme via JNI.
/// Returns: 0 = no preference, 1 = prefer dark, 2 = prefer light.
pub fn get_android_color_scheme() -> u32 {
    jni_context::with_jni(|env, activity| {
        let resources = env
            .call_method(activity, "getResources", "()Landroid/content/res/Resources;", &[])?
            .l()?;
        let config = env
            .call_method(&resources, "getConfiguration", "()Landroid/content/res/Configuration;", &[])?
            .l()?;
        let ui_mode = env.get_field(&config, "uiMode", "I")?.i()?;

        // UI_MODE_NIGHT_MASK = 0x30, UI_MODE_NIGHT_YES = 0x20, UI_MODE_NIGHT_NO = 0x10
        let night = ui_mode & 0x30;
        let scheme = match night {
            0x20 => 1u32, // dark
            0x10 => 2u32, // light
            _ => 0u32,    // no preference
        };
        tracing::debug!("Android color scheme: uiMode=0x{ui_mode:x} → {scheme}");
        Ok(scheme)
    })
    .unwrap_or(0)
}

/// Called from the compositor event handler when a PortalRequest arrives.
/// Launches the FileChooserActivity via JNI.
pub fn handle_portal_request(request: &PortalRequest) {
    let id = request.id.clone();
    let request_type = match request.request_type {
        PortalRequestType::OpenFile => "open_file",
        PortalRequestType::SaveFile => "save_file",
    };
    let title = request.title.clone();
    let multiple = request.multiple;
    let directory = request.directory;
    let mime_types = request.mime_types.join(",");
    let current_name = request.current_name.clone().unwrap_or_default();

    if let Err(e) = launch_file_chooser(
        &id,
        request_type,
        &title,
        multiple,
        directory,
        &mime_types,
        &current_name,
    ) {
        tracing::error!("Failed to launch FileChooserActivity: {e}");
        // Send error response.
        let _ = request.response_tx.send(PortalResponse {
            id: request.id.clone(),
            response: 2,
            uris: vec![],
        });
    }
}

fn launch_file_chooser(
    request_id: &str,
    request_type: &str,
    title: &str,
    multiple: bool,
    directory: bool,
    mime_types: &str,
    current_name: &str,
) -> Result<(), jni::errors::Error> {
    jni_context::with_jni(|env, activity| {
        let chooser_class = jni_context::load_class(
            env,
            activity,
            "io.github.phiresky.wayland_android.FileChooserActivity",
        )?;

        let intent_class = env.find_class("android/content/Intent")?;
        let intent = env.new_object(
            &intent_class,
            "(Landroid/content/Context;Ljava/lang/Class;)V",
            &[
                JValue::Object(activity),
                JValue::Object(&chooser_class.into()),
            ],
        )?;

        // Put extras
        let mut put_string = |key: &str, val: &str| -> Result<(), jni::errors::Error> {
            let k = env.new_string(key)?;
            let v = env.new_string(val)?;
            env.call_method(
                &intent,
                "putExtra",
                "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
                &[JValue::Object(&k), JValue::Object(&v)],
            )?;
            Ok(())
        };

        put_string("request_id", request_id)?;
        put_string("request_type", request_type)?;
        put_string("title", title)?;
        put_string("mime_types", mime_types)?;
        put_string("current_name", current_name)?;

        // Boolean extras
        let mut put_bool = |key: &str, val: bool| -> Result<(), jni::errors::Error> {
            let k = env.new_string(key)?;
            env.call_method(
                &intent,
                "putExtra",
                "(Ljava/lang/String;Z)Landroid/content/Intent;",
                &[JValue::Object(&k), JValue::Bool(if val { 1 } else { 0 })],
            )?;
            Ok(())
        };

        put_bool("multiple", multiple)?;
        put_bool("directory", directory)?;

        // Launch as new task
        const FLAG_ACTIVITY_NEW_TASK: i32 = 0x10000000;
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(FLAG_ACTIVITY_NEW_TASK)],
        )?;

        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;

        tracing::info!("Launched FileChooserActivity for request {request_id}");
        Ok(())
    })
}

/// JNI callback: FileChooserActivity completed.
/// Called from Kotlin with the request ID and comma-separated file paths (in rootfs).
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_FileChooserActivity_nativeFileChooserResult(
    mut env: jni::JNIEnv,
    _class: jni::objects::JObject,
    request_id: jni::objects::JString,
    response_code: i32,
    paths: jni::objects::JString,
) {
    let request_id = crate::android::utils::jni_context::get_string(&mut env, &request_id);
    let paths_str = crate::android::utils::jni_context::get_string(&mut env, &paths);

    let uris: Vec<String> = if paths_str.is_empty() {
        vec![]
    } else {
        paths_str
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(|s| format!("file://{s}"))
            .collect()
    };

    tracing::info!(
        "FileChooser result: id={request_id} response={response_code} uris={uris:?}"
    );

    let response = PortalResponse {
        id: request_id.clone(),
        response: response_code as u32,
        uris,
    };

    // Find and signal the pending request.
    if let Ok(mut guard) = PENDING.lock() {
        if let Some(map) = guard.as_mut() {
            if let Some(tx) = map.remove(&request_id) {
                let _ = tx.send(response);
                return;
            }
        }
    }
    tracing::warn!("No pending portal request for id={request_id}");
}
