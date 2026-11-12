use crate::{
    AssertionResult, Captured, HttpClient, NamedScripts, Request, Response, SendError, Streamed,
    Variables, capture, check, post_response, pre_request,
};

/// One request sent with everything around it: scripts, variables, captures and tests.
#[derive(Debug)]
pub struct Exchange {
    /// As sent, with its variables resolved.
    pub request: Request,
    pub unresolved: Vec<String>,
    pub outcome: Result<Response, SendError>,
    /// The variables the pre-request scripts set, then the captures, then the variables the
    /// post-response scripts set.
    pub captured: Vec<Captured>,
    /// The assertions, then the tests of the scripts.
    pub assertions: Vec<AssertionResult>,
    /// What the scripts printed.
    pub logs: Vec<String>,
}

impl Exchange {
    pub fn passed(&self) -> bool {
        self.outcome
            .as_ref()
            .is_ok_and(|response| crate::passed(response, &self.assertions))
    }
}

fn set(variables: Vec<(String, String)>) -> impl Iterator<Item = Captured> {
    variables.into_iter().map(|(variable, value)| Captured {
        variable,
        value: Ok(value),
    })
}

/// Runs the pre-request scripts, resolves and sends the request, then captures, checks and runs
/// the post-response scripts. Streams end when `stop` completes, as with `send_streaming`.
pub async fn exchange(
    client: &HttpClient,
    request: &Request,
    variables: &Variables,
    scripts: &[NamedScripts],
    stop: impl Future<Output = ()>,
    on_update: impl FnMut(Streamed),
) -> Exchange {
    let mut request = request.clone();
    let mut variables = variables.clone();
    let mut captured = Vec::new();
    let mut assertions = Vec::new();
    let mut logs = Vec::new();

    let pre: Vec<_> = scripts
        .iter()
        .filter(|named| !named.scripts.pre.trim().is_empty())
        .collect();
    let mut failure = None;
    if !pre.is_empty() {
        let output = pre_request(&pre, &mut request, &variables);
        variables.push_layer(output.variables.clone());
        captured.extend(set(output.variables));
        assertions.extend(output.tests);
        logs.extend(output.logs);
        failure = output.error;
    }
    let resolved = request.resolve(&variables);
    if let Some(error) = failure {
        return Exchange {
            request: resolved.request,
            unresolved: resolved.unresolved,
            outcome: Err(SendError::Script(error)),
            captured,
            assertions,
            logs,
        };
    }

    let outcome = send(client, &resolved.request, stop, on_update).await;
    if let Ok(response) = &outcome {
        let captures = capture(&resolved.request.captures, response);
        variables.push_layer(
            captures
                .iter()
                .filter_map(|c| Some((c.variable.clone(), c.value.clone().ok()?))),
        );
        captured.extend(captures);
        assertions.extend(check(&resolved.request.assertions, response));
        let post: Vec<_> = scripts
            .iter()
            .filter(|named| !named.scripts.post.trim().is_empty())
            .collect();
        if !post.is_empty() {
            let output = post_response(&post, &resolved.request, response, &variables);
            captured.extend(set(output.variables));
            assertions.extend(output.tests);
            logs.extend(output.logs);
            if let Some(error) = output.error {
                assertions.push(AssertionResult {
                    description: "post-response script".into(),
                    actual: None,
                    error: Some(error),
                });
            }
        }
    }
    Exchange {
        request: resolved.request,
        unresolved: resolved.unresolved,
        outcome,
        captured,
        assertions,
        logs,
    }
}

async fn send(
    client: &HttpClient,
    request: &Request,
    stop: impl Future<Output = ()>,
    on_update: impl FnMut(Streamed),
) -> Result<Response, SendError> {
    if crate::is_websocket_url(&request.url) {
        return Err(SendError::WebSocket(
            "WebSocket requests are opened with Connect, they cannot be sent".into(),
        ));
    }
    if crate::is_grpc_url(&request.url) {
        client.grpc_call(request, stop, on_update).await
    } else {
        client.send_streaming(request, stop, on_update).await
    }
}
