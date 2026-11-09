use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, TryRecvError};

use cornu_core::{Body, GrpcMethod, SendError};
use eframe::egui;

use crate::websocket::{message, set_message};

type Methods = Result<Vec<GrpcMethod>, String>;

/// The methods offered by the server, asked through reflection when the picker first opens.
#[derive(Default)]
pub struct MethodPicker {
    methods: Option<Methods>,
    pending: Option<Receiver<Result<Vec<GrpcMethod>, SendError>>>,
    /// The server the methods came from, to ask again when the URL changes.
    server: String,
}

impl MethodPicker {
    pub fn start(&mut self, pending: Receiver<Result<Vec<GrpcMethod>, SendError>>, server: &str) {
        self.pending = Some(pending);
        self.methods = None;
        self.server = server.to_owned();
    }

    fn poll(&mut self) {
        let Some(pending) = &self.pending else {
            return;
        };
        match pending.try_recv() {
            Ok(result) => {
                self.methods = Some(result.map_err(|error| error.to_string()));
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.methods = Some(Err("the request was cancelled".into()));
                self.pending = None;
            }
        }
    }

    /// Returns true when the methods of `server` need to be asked for.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        resolved_url: &str,
        url: &mut String,
        body: &mut Body,
    ) -> bool {
        self.poll();
        let server = base(resolved_url);
        let mut fetch = false;
        let current = method_path(url).unwrap_or("Pick a method");
        ui.menu_button(format!("{current} ⏷"), |ui| {
            if self.pending.is_none() && (self.methods.is_none() || self.server != server) {
                fetch = true;
            }
            ui.set_min_width(260.0);
            match (&self.pending, &self.methods) {
                (Some(_), _) | (None, None) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Asking the server through reflection…");
                    });
                }
                (None, Some(Err(error))) => {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                (None, Some(Ok(methods))) if methods.is_empty() => {
                    ui.weak("The server lists no service.");
                }
                (None, Some(Ok(methods))) => {
                    let mut services: BTreeMap<&str, Vec<&GrpcMethod>> = BTreeMap::new();
                    for method in methods {
                        services.entry(&method.service).or_default().push(method);
                    }
                    for (service, methods) in services {
                        ui.label(egui::RichText::new(service).strong());
                        for method in methods {
                            let label = format!("{}{}", method.name, streaming_label(method));
                            if ui
                                .selectable_label(
                                    Some(method.path().as_str()) == method_path(url),
                                    label,
                                )
                                .clicked()
                            {
                                pick(url, body, method);
                                ui.close();
                            }
                        }
                    }
                }
            }
            if self.pending.is_none() {
                ui.separator();
                if ui.button("⟳ Refresh").clicked() {
                    fetch = true;
                }
            }
        });
        fetch
    }
}

fn streaming_label(method: &GrpcMethod) -> &'static str {
    match (method.client_streaming, method.server_streaming) {
        (false, false) => "",
        (false, true) => "  · server stream",
        (true, false) => "  · client stream",
        (true, true) => "  · bidirectional",
    }
}

/// The `package.Service/Method` part of a gRPC URL.
fn method_path(url: &str) -> Option<&str> {
    let start = url.find("://").map_or(0, |index| index + 3);
    let path = url[start..].split_once('/')?.1.trim_matches('/');
    (!path.is_empty()).then_some(path)
}

/// The URL without its method.
pub fn base(url: &str) -> &str {
    let start = url.find("://").map_or(0, |index| index + 3);
    match url[start..].find('/') {
        Some(index) => &url[..start + index],
        None => url.trim_end(),
    }
}

fn pick(url: &mut String, body: &mut Body, method: &GrpcMethod) {
    *url = format!("{}/{}", base(url), method.path());
    let current = message(body).trim();
    if current.is_empty() || current == "{}" || current == "[]" {
        set_message(body, method.template.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(service: &str, name: &str) -> GrpcMethod {
        GrpcMethod {
            service: service.into(),
            name: name.into(),
            client_streaming: false,
            server_streaming: true,
            template: "{\n  \"n\": 0\n}".into(),
        }
    }

    #[test]
    fn picking_a_method_sets_the_url_path_and_a_template() {
        let mut url = "grpc://localhost:50051".to_owned();
        let mut body = Body::None;
        pick(&mut url, &mut body, &method("test.Greeter", "Count"));
        assert_eq!(url, "grpc://localhost:50051/test.Greeter/Count");
        assert_eq!(message(&body), "{\n  \"n\": 0\n}");
        assert_eq!(method_path(&url), Some("test.Greeter/Count"));

        set_message(&mut body, "{\"n\": 3}".into());
        let mut url = "{{server}}/old.Service/Call".to_owned();
        pick(&mut url, &mut body, &method("test.Greeter", "SayHello"));
        assert_eq!(url, "{{server}}/test.Greeter/SayHello");
        assert_eq!(message(&body), "{\"n\": 3}");
        assert_eq!(method_path("grpcs://{{host}}"), None);
    }

    #[test]
    fn methods_are_listed_by_service() {
        let mut picker = MethodPicker::default();
        let (tx, rx) = std::sync::mpsc::channel();
        picker.start(rx, "grpc://localhost:50051");
        tx.send(Ok(vec![
            method("test.Greeter", "Count"),
            method("test.Admin", "Reset"),
        ]))
        .unwrap();
        picker.poll();
        assert_eq!(picker.methods.as_ref().unwrap().as_ref().unwrap().len(), 2);
        assert_eq!(streaming_label(&method("a", "b")), "  · server stream");
    }
}
