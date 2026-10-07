// 模型容量单位及自动填充：token 不是字节，K/M 使用十进制，无浮点舍入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AiTokenUnit {
    K,
    M,
}
impl AiTokenUnit {
    fn from_label(label: &str) -> Self {
        if label == "M" { Self::M } else { Self::K }
    }
    fn label(self) -> &'static str {
        match self {
            Self::K => "K",
            Self::M => "M",
        }
    }
    fn scale(self) -> i64 {
        match self {
            Self::K => 1000,
            Self::M => 1_000_000,
        }
    }
}

fn ai_parse_token_count(raw: &str, unit: AiTokenUnit) -> Result<Option<i64>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.is_empty()
        || !whole.bytes().all(|c| c.is_ascii_digit())
        || !fraction.bytes().all(|c| c.is_ascii_digit())
    {
        return Err("容量必须是正数，可输入小数 K/M tokens".into());
    }
    let precision = match unit {
        AiTokenUnit::K => 3,
        AiTokenUnit::M => 6,
    };
    if fraction.len() > precision && fraction.as_bytes()[precision..].iter().any(|c| *c != b'0') {
        return Err("换算后必须是整数 tokens".into());
    }
    let part = &fraction[..fraction.len().min(precision)];
    let whole = whole
        .parse::<i64>()
        .map_err(|_| "容量超出范围".to_string())?;
    let frac = if part.is_empty() {
        0
    } else {
        part.parse::<i64>()
            .map_err(|_| "容量格式无效".to_string())?
    } * 10i64.pow((precision - part.len()) as u32);
    let value = whole
        .checked_mul(unit.scale())
        .and_then(|n| n.checked_add(frac))
        .filter(|v| (1..=1_000_000_000).contains(v))
        .ok_or_else(|| "容量必须为 1–1,000,000,000 tokens".to_string())?;
    Ok(Some(value))
}

fn ai_format_token_count(tokens: i64, unit: AiTokenUnit) -> String {
    let scale = unit.scale();
    let whole = tokens / scale;
    let remainder = tokens % scale;
    if remainder == 0 {
        return whole.to_string();
    }
    let digits = if unit == AiTokenUnit::K { 3 } else { 6 };
    format!("{whole}.{:0digits$}", remainder)
        .trim_end_matches('0')
        .to_string()
}

fn ai_capacity_field(
    label: &str,
    input: &Entity<InputState>,
    unit: &Entity<SelectState<SearchableVec<String>>>,
    colors: UiColors,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .min_w(px(0.))
        .child(
            div()
                .text_size(px(11.))
                .text_color(colors.text)
                .child(label.to_string()),
        )
        .child(
            div()
                .flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .debug_selector(|| format!("ai-field:{label}"))
                        .child(Input::new(input).text_size(px(13.))),
                )
                .child(
                    div()
                        .w(px(70.))
                        .flex_none()
                        .debug_selector(|| format!("ai-capacity-unit:{label}"))
                        .child(Select::new(unit).w_full().h(px(32.))),
                ),
        )
}

impl NavicatMain {
    fn change_ai_token_unit(
        &mut self,
        index: usize,
        next: AiTokenUnit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.ai_model_dialog.as_mut() else {
            return;
        };
        let previous = dialog.capacity_units[index];
        if previous == next {
            return;
        }
        let input = if index == 0 {
            &dialog.inputs.context_window
        } else {
            &dialog.inputs.max_output
        };
        match ai_parse_token_count(&ai_input_text(input, cx), previous) {
            Ok(tokens) => {
                let text = tokens
                    .map(|n| ai_format_token_count(n, next))
                    .unwrap_or_default();
                if dialog.capacity_autofill[index].as_deref()
                    == Some(ai_input_text(input, cx).as_str())
                {
                    dialog.capacity_autofill[index] = Some(text.clone());
                }
                input.update(cx, |state, cx| state.set_value(text, window, cx));
                dialog.capacity_units[index] = next;
            }
            Err(message) => {
                let select = if index == 0 {
                    &dialog.inputs.context_unit
                } else {
                    &dialog.inputs.output_unit
                };
                select.update(cx, |state, cx| {
                    state.set_selected_value(&previous.label().to_string(), window, cx)
                });
                dialog.validation_error = Some(message);
            }
        }
        cx.notify();
    }

    fn apply_ai_model_capacity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.ai_model_dialog.as_mut() else {
            return;
        };
        let selected = ai_input_text(&dialog.inputs.model_key, cx);
        let metadata = dialog
            .model_capabilities
            .iter()
            .find(|m| m.id == selected)
            .cloned();
        for (index, input) in [&dialog.inputs.context_window, &dialog.inputs.max_output]
            .into_iter()
            .enumerate()
        {
            let current = ai_input_text(input, cx);
            // 手动输入优先；切模型时只替换仍未被编辑的自动值。
            if !current.is_empty()
                && dialog.capacity_autofill[index].as_deref() != Some(current.as_str())
            {
                continue;
            }
            let tokens = metadata.as_ref().and_then(|m| {
                if index == 0 {
                    m.context_window_tokens
                } else {
                    m.max_output_tokens
                }
            });
            let text = tokens
                .map(|n| ai_format_token_count(n, dialog.capacity_units[index]))
                .unwrap_or_default();
            if current != text {
                input.update(cx, |state, cx| state.set_value(text.clone(), window, cx));
            }
            dialog.capacity_autofill[index] = tokens.map(|_| text);
        }
        cx.notify();
    }
}
