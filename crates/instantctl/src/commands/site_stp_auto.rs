use clap::Args as ClapArgs;
use instantctl_api::client::site_actions::BridgePriorityAction;
use serde_json::{Value, json};

use crate::{
    context::{CommandContext, CommandResult},
    exit::ExitStatus,
    mutation::Options,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(flatten)]
    pub options: Options,
}

pub async fn run(args: Args, context: &CommandContext) -> anyhow::Result<CommandResult> {
    args.options.preflight(context.token_source)?;
    let client = super::site::read::client(context)?;
    let site = super::site::read::site_id(&client, context).await?;
    let action = BridgePriorityAction::prepare(&client, &site).await?;
    let mut data = json!({
        "operation":"site.stp_auto_priority",
        "target":{"site_id":site},
        "current":action.current,
        "desired":{"action":"computeDevicesBridgePriority","completion_verifiable":false},
        "request_attempted":false,
    });
    crate::output::write_data(&mut std::io::stderr().lock(), context.format, &data)?;
    if !args.options.apply {
        return Ok(CommandResult::success(data));
    }
    crate::mutation::confirm(args.options.yes)?;
    let report = action.apply(context.timeout).await?;
    let status = report
        .error_kind()
        .map_or(ExitStatus::Success, ExitStatus::Error);
    let object = data
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("action plan must be an object"))?;
    object.insert("request_attempted".into(), Value::Bool(true));
    let Value::Object(report) = serde_json::to_value(report)? else {
        anyhow::bail!("action report must be an object");
    };
    object.extend(report);
    Ok(CommandResult { data, status })
}
