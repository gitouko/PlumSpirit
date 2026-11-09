//! Plugin-manager side of bounded service routing. No Lua executes here.
use super::*;
use crate::lua::service_calls::{Failure, MAX_PER_PROVIDER, Request};
use crate::lua::{Capability, Category};
use std::sync::Arc;

impl PluginManager {
    pub(super) fn poll_service_calls(&mut self, prefs: &PluginPrefs) {
        let current = self.observation_stamp;
        self.service_graph.clean(current);
        let graph = &mut self.service_graph;
        let mut incoming = Vec::new();
        for p in &mut self.plugins {
            if let Some(r) = p.runner.as_mut() {
                if let Some(Pending::Service {
                    request: Some(request),
                    ..
                }) = &r.running
                    && !request.frame.matches(current)
                {
                    request.fence.invalidate();
                    r.worker.cancel();
                }
                r.queue.retain_mut(|pending| {
                    if let Pending::Service {
                        call_id, request, ..
                    } = pending
                        && request.as_ref().is_none_or(|q| !q.frame.matches(current))
                    {
                        graph.complete(*call_id);
                        if let Some(request) = request.take() {
                            request.respond(Err(Failure::new(
                                Category::Expired,
                                "The queued service call expired.",
                            )));
                        }
                        return false;
                    }
                    true
                });
                if let Some(request) = r.worker.poll_service_request() {
                    let ready = p.status == Status::Ready && r.ready;
                    incoming.push((p.id.clone(), Arc::clone(&r.lifetime), ready, request));
                }
            }
            if let Some(r) = p.candidate.as_ref()
                && let Some(request) = r.worker.poll_service_request()
            {
                request.respond(Err(Failure::new(
                    Category::Capability,
                    "Loading service clients are not admitted.",
                )));
            }
            for r in &p.retiring {
                if let Some(request) = r.worker.poll_service_request() {
                    request.respond(Err(Failure::new(
                        Category::Expired,
                        "The service caller was retired.",
                    )));
                }
            }
        }
        for (owner, life, ready, mut request) in incoming {
            if !ready || request.caller.as_ref() != owner || !life.active() {
                request.respond(Err(Failure::new(
                    Category::Expired,
                    "The service caller is not an active ready plugin.",
                )));
                continue;
            }
            request.frame.lives.push(life);
            if let Err(error) = self.admit_service(&mut request, prefs) {
                request.respond(Err(error));
                continue;
            }
            // Admission installed a call ID; move the request into its provider queue.
            let (target, local) = crate::lua::service_calls::split_namespace(&request.namespace)
                .expect("validated namespace");
            let target = target.to_owned();
            let local = local.to_owned();
            let p = self.plugin_mut(&target).expect("admitted provider");
            let r = p.runner.as_mut().expect("admitted runner");
            let version = request.requested.expect("resolved dependency version");
            let id = request.frame.calls_id();
            let job = crate::lua::services::Job {
                id: local,
                version,
                data: request.data.clone(),
                lifecycle: Arc::clone(&r.lifetime),
                seed: request.seed,
                call: Some(request.frame.clone()),
            };
            r.queue.push_back(Pending::Service {
                call_id: id,
                job,
                request: Some(request),
            });
        }
    }
    fn admit_service(&mut self, request: &mut Request, prefs: &PluginPrefs) -> Result<(), Failure> {
        if !request.frame.matches(self.observation_stamp) {
            return Err(Failure::new(
                Category::Expired,
                "The service caller/project revision expired.",
            ));
        }
        let (target, local) = crate::lua::service_calls::split_namespace(&request.namespace)
            .map_err(|e| Failure::new(Category::Runtime, e))?;
        let target = target.to_owned();
        let local = local.to_owned();
        let caller = self
            .plugin(request.caller.as_ref())
            .ok_or_else(|| Failure::new(Category::Expired, "The caller was removed."))?;
        let caller_state = prefs.installed.get(&caller.id);
        if !prefs.run_plugins
            || !caller_state.is_some_and(|s| s.enabled)
            || !caller
                .effective(caller_state)
                .contains(&Capability::CallServices)
            || !request.frame.authority.contains(&Capability::CallServices)
        {
            return Err(Failure::new(
                Category::Capability,
                "The caller lacks services.call authority.",
            ));
        }
        if request
            .frame
            .path
            .last()
            .is_none_or(|p| p != request.caller.as_ref())
        {
            return Err(Failure::new(
                Category::Expired,
                "The caller's service chain changed.",
            ));
        }
        let dep = caller
            .manifest
            .as_ref()
            .ok()
            .and_then(|m| {
                m.service_dependencies
                    .iter()
                    .find(|d| d.namespace == request.namespace)
            })
            .ok_or_else(|| {
                Failure::new(
                    Category::Capability,
                    "This service is not declared in the caller's service_dependencies.",
                )
            })?;
        let version = request.requested.unwrap_or(dep.version);
        if version.major != dep.version.major || version.minor < dep.version.minor {
            return Err(Failure::new(
                Category::Compatibility,
                "The requested service version conflicts with its declared dependency.",
            ));
        }
        let provider = self.plugin(&target).ok_or_else(|| {
            Failure::new(
                Category::Runtime,
                "The declared service provider is not installed.",
            )
        })?;
        let state = prefs.installed.get(&target);
        let caps = provider.effective(state);
        if provider.status != Status::Ready
            || !state.is_some_and(|s| s.enabled)
            || !caps.contains(&Capability::ProvideServices)
        {
            return Err(Failure::new(
                Category::Capability,
                "The declared service provider is not enabled/ready or lacks services.provide.",
            ));
        }
        let runner = provider
            .runner
            .as_ref()
            .ok_or_else(|| Failure::new(Category::Expired, "The provider worker stopped."))?;
        let decl = runner
            .decls
            .services
            .iter()
            .find(|d| d.id == local)
            .ok_or_else(|| {
                Failure::new(
                    Category::Runtime,
                    "The declared provider has no such service.",
                )
            })?;
        if !decl.version.supports(version) {
            return Err(Failure::new(
                Category::Compatibility,
                "The provider's service version is incompatible.",
            ));
        }
        let pending = runner
            .queue
            .iter()
            .filter(|p| matches!(p, Pending::Service { .. }))
            .count()
            + usize::from(matches!(runner.running, Some(Pending::Service { .. })));
        if pending >= MAX_PER_PROVIDER {
            return Err(Failure::new(
                Category::Budget,
                "The provider's eight-call queue budget is full.",
            ));
        }
        let life = Arc::clone(&runner.lifetime);
        request.frame.lives.push(life);
        request.frame.authority = Arc::new(
            request
                .frame
                .authority
                .iter()
                .copied()
                .filter(|c| caps.contains(c))
                .collect(),
        );
        let id = self
            .service_graph
            .admit(request.caller.as_ref(), &target, &request.frame)?;
        request.frame.path.push(target);
        request.requested = Some(version);
        request.frame.set_calls_id(id);
        Ok(())
    }
    pub(super) fn complete_service(
        &mut self,
        provider: &str,
        id: u64,
        job: crate::lua::services::Job,
        request: Request,
        result: ScriptResult,
        prefs: &PluginPrefs,
    ) {
        self.service_graph.complete(id);
        let allowed = request.frame.matches(self.observation_stamp)
            && job.lifecycle.active()
            && prefs.run_plugins
            && request
                .frame
                .path
                .iter()
                .take(request.frame.path.len().saturating_sub(1))
                .all(|owner| {
                    self.plugin(owner).is_some_and(|p| {
                        p.status == Status::Ready
                            && prefs.installed.get(owner).is_some_and(|s| s.enabled)
                            && p.effective(prefs.installed.get(owner))
                                .contains(&Capability::CallServices)
                    })
                })
            && self.plugin(provider).is_some_and(|p| {
                p.status == Status::Ready
                    && prefs.installed.get(provider).is_some_and(|s| s.enabled)
                    && p.effective(prefs.installed.get(provider))
                        .contains(&Capability::ProvideServices)
            });
        if !allowed {
            request.respond(Err(Failure::new(
                Category::Expired,
                "The service result's authority, lifecycle or project revision expired.",
            )));
            return;
        }
        if let Some(p) = self.plugin_mut(provider) {
            for line in result.output {
                p.log.push(Level::Output, line);
            }
        }
        let reply = match result.result {
            Ok(_) => result.service_reply.ok_or_else(|| {
                Failure::new(
                    Category::Internal,
                    "The provider returned no bounded service reply.",
                )
            }),
            Err(error) => Err(Failure::new(
                error.category,
                format!("Service {}: {}", request.namespace, error.message),
            )),
        };
        // A pure provider cannot publish project/UI/config effects here.
        request.respond(reply);
    }
}
