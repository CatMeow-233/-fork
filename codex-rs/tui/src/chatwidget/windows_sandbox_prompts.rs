//! Windows sandbox prompts and warning surfaces for `ChatWidget`.

use super::*;
#[cfg(any(target_os = "windows", test))]
use crate::render::renderable::ColumnRenderable;

impl ChatWidget {
    #[cfg(any(target_os = "windows", test))]
    pub(super) fn elevated_windows_sandbox_setup_required(&self) -> bool {
        self.windows_sandbox_config.requires_elevated()
            && (self.windows_sandbox_host != crate::app::WindowsSandboxHost::Local
                || !self.windows_sandbox_elevated_setup_complete)
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn open_windows_sandbox_enable_prompt(
        &mut self,
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
    ) {
        use ratatui_macros::line;

        self.session_telemetry.counter(
            "codex.windows_sandbox.elevated_prompt_shown",
            /*inc*/ 1,
            &[],
        );

        let header = if self
            .windows_sandbox_config
            .allows(WindowsSandboxSetupMode::Unelevated)
        {
            vec![line![
                "Set up the Codex agent sandbox to protect your files and control network access. Learn more <https://developers.openai.com/codex/windows>"
            ]]
        } else {
            vec![
                line![
                    "Your organization requires the default Codex agent sandbox to continue. Set it up to protect your files and control network access."
                ],
                line!["Learn more <https://developers.openai.com/codex/windows>"],
            ]
        };
        self.show_windows_sandbox_setup_choices(
            header,
            preset,
            profile_selection,
            [
                (
                    "Set up default sandbox (requires Administrator permissions)",
                    "codex.windows_sandbox.elevated_prompt_accept",
                ),
                (
                    "Use non-admin sandbox (higher risk if prompt injected)",
                    "codex.windows_sandbox.elevated_prompt_use_legacy",
                ),
                ("Quit", "codex.windows_sandbox.elevated_prompt_quit"),
            ],
            |preset, profile_selection| AppEvent::OpenWindowsSandboxEnablePrompt {
                preset,
                profile_selection,
            },
        );
    }

    #[cfg(all(not(target_os = "windows"), not(test)))]
    pub(crate) fn open_windows_sandbox_enable_prompt(
        &mut self,
        _preset: ApprovalPreset,
        _profile_selection: Option<PermissionProfileSelection>,
    ) {
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn open_windows_sandbox_fallback_prompt(
        &mut self,
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
    ) {
        use ratatui_macros::line;

        let mut lines = vec![
            line!["Couldn't set up your sandbox with Administrator permissions".bold()],
            line![""],
        ];
        if self
            .windows_sandbox_config
            .allows(WindowsSandboxSetupMode::Unelevated)
        {
            lines.push(line![
                "You can still use Codex in a non-admin sandbox. It carries greater risk if prompt injected."
            ]);
        } else {
            lines.push(line![
                "Your organization requires the default sandbox before Codex can continue."
            ]);
        }
        lines.push(line![
            "Learn more <https://developers.openai.com/codex/windows>"
        ]);
        self.show_windows_sandbox_setup_choices(
            lines,
            preset,
            profile_selection,
            [
                (
                    "Try setting up admin sandbox again",
                    "codex.windows_sandbox.fallback_retry_elevated",
                ),
                (
                    "Use Codex with non-admin sandbox",
                    "codex.windows_sandbox.fallback_use_legacy",
                ),
                ("Quit", "codex.windows_sandbox.fallback_prompt_quit"),
            ],
            |preset, profile_selection| AppEvent::OpenWindowsSandboxFallbackPrompt {
                preset,
                profile_selection,
            },
        );
    }

    #[cfg(all(not(target_os = "windows"), not(test)))]
    pub(crate) fn open_windows_sandbox_fallback_prompt(
        &mut self,
        _preset: ApprovalPreset,
        _profile_selection: Option<PermissionProfileSelection>,
    ) {
    }

    /// Shows the elevated, non-admin, and quit choices shared by both setup prompts.
    #[cfg(any(target_os = "windows", test))]
    fn show_windows_sandbox_setup_choices(
        &mut self,
        header_lines: Vec<Line<'static>>,
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
        [elevated, legacy, quit]: [(&str, &'static str); 3],
        reopen: fn(ApprovalPreset, Option<PermissionProfileSelection>) -> AppEvent,
    ) {
        let allow_unelevated = self
            .windows_sandbox_config
            .allows(WindowsSandboxSetupMode::Unelevated);
        let setup_choice_is_required =
            !allow_unelevated || self.elevated_windows_sandbox_setup_required();
        let mut items = Vec::new();
        if self
            .windows_sandbox_config
            .allows(WindowsSandboxSetupMode::Elevated)
        {
            let (preset, profile_selection) = (preset.clone(), profile_selection.clone());
            items.push(self.windows_sandbox_choice(elevated, move || {
                AppEvent::BeginWindowsSandboxElevatedSetup {
                    preset: preset.clone(),
                    profile_selection: profile_selection.clone(),
                }
            }));
        }
        if allow_unelevated {
            let (preset, profile_selection) = (preset.clone(), profile_selection.clone());
            items.push(SelectionItem {
                require_explicit_confirmation: true,
                ..self.windows_sandbox_choice(legacy, move || {
                    AppEvent::BeginWindowsSandboxLegacySetup {
                        preset: preset.clone(),
                        profile_selection: profile_selection.clone(),
                    }
                })
            });
        }
        items.push(self.windows_sandbox_choice(quit, || AppEvent::Exit(ExitMode::ShutdownFirst)));

        let mut header = ColumnRenderable::new();
        header.push(crate::terminal_hyperlinks::HyperlinkText::new(header_lines));
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            on_cancel: setup_choice_is_required.then(|| {
                Box::new(move |tx: &AppEventSender| {
                    tx.send(reopen(preset.clone(), profile_selection.clone()));
                }) as _
            }),
            ..SelectionViewParams::picker()
        });
    }

    /// One prompt row that records `metric` before sending `event`.
    #[cfg(any(target_os = "windows", test))]
    fn windows_sandbox_choice(
        &self,
        (name, metric): (&str, &'static str),
        event: impl Fn() -> AppEvent + Send + Sync + 'static,
    ) -> SelectionItem {
        let otel = self.session_telemetry.clone();
        SelectionItem {
            name: name.to_string(),
            actions: vec![Box::new(move |tx| {
                otel.counter(metric, /*inc*/ 1, &[]);
                tx.send(event());
            })],
            dismiss_on_select: true,
            ..Default::default()
        }
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn maybe_prompt_windows_sandbox_enable(&mut self, show_now: bool) {
        let setup_is_required = !self.windows_sandbox_config.is_enabled()
            || self.elevated_windows_sandbox_setup_required();
        if show_now
            && setup_is_required
            && let Some(mut preset) = builtin_approval_presets()
                .into_iter()
                .find(|preset| preset.id == "auto")
        {
            if self.config.active_project.trust_level.is_none()
                && self.config.config_layer_stack.is_projectless()
            {
                preset.approval = self.config.permissions.approval_policy.value();
            }
            self.open_windows_sandbox_enable_prompt(preset, /*profile_selection*/ None);
        }
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn show_windows_sandbox_setup_status(&mut self) {
        // While elevated sandbox setup runs, prevent typing so the user doesn't
        // accidentally queue messages that will run under an unexpected mode.
        self.bottom_pane.set_composer_input_enabled(
            /*enabled*/ false,
            Some("Input disabled until setup completes.".to_string()),
        );
        self.bottom_pane.reset_status_timer(Duration::ZERO);
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane
            .set_interrupt_hint_visible(/*visible*/ false);
        self.set_status(
            "Setting up sandbox...".to_string(),
            Some("Hang tight, this may take a few minutes".to_string()),
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
        self.request_redraw();
    }

    #[cfg(not(any(target_os = "windows", test)))]
    #[allow(dead_code)]
    pub(crate) fn show_windows_sandbox_setup_status(&mut self) {}

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn clear_windows_sandbox_setup_status(&mut self) {
        self.bottom_pane
            .set_composer_input_enabled(/*enabled*/ true, /*placeholder*/ None);
        self.bottom_pane.hide_status_indicator();
        self.request_redraw();
    }

    #[cfg(not(any(target_os = "windows", test)))]
    pub(crate) fn clear_windows_sandbox_setup_status(&mut self) {}
}
