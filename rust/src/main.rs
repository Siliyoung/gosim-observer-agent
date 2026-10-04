//! Entry point: reads `participant-agent-protocol-v4` JSON Lines from stdin,
//! writes exactly one `decision_response` per `decision_request` to stdout,
//! and sends everything else (startup notes, learning updates, LLM call
//! outcomes) to stderr. See `README.md` for the module map.

mod llm;
mod memory;
mod planner;
mod protocol;
mod scoring;
mod state;
mod validate;

use std::io::{self, BufRead, Write};

use memory::{log, Memory};
use protocol::Inbound;
use state::{Config, RunState};

fn main() {
    let llm_client = match llm::LlmClient::from_env() {
        Ok(client) => client,
        Err(message) => {
            log(&message);
            std::process::exit(1);
        }
    };
    log(&format!("agent: LLM endpoint {} model {}", llm_client.base_url(), llm_client.model()));

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    log("agent: starting, waiting for initialize");
    let config = match wait_for_initialize(&mut reader) {
        Some(config) => config,
        None => return,
    };
    log(&format!(
        "agent: initialized with {} target(s), {} night(s), {} fibre(s), wallclock budget {:.0}s",
        config.targets.len(),
        config.nights.len(),
        config.grid.n_fibers(),
        config.global_wallclock_seconds
    ));

    let mut run = RunState::new(config.global_wallclock_seconds);
    let mut memory = Memory::new(&config);

    run_decision_loop(&mut reader, &mut writer, &config, &mut run, &mut memory, &llm_client);
}

/// Reads lines until `initialize` arrives (ignoring anything before it, which
/// the protocol never actually sends but which must not crash us either), or
/// the stream ends first.
fn wait_for_initialize<R: BufRead>(reader: &mut R) -> Option<Config> {
    loop {
        match protocol::read_message(reader) {
            Ok(Some(Inbound::Initialize(payload))) => return Some(Config::from_init(&payload)),
            Ok(Some(Inbound::Unrecognized(reason))) => log(&format!("agent: ignoring pre-initialize line ({reason})")),
            Ok(Some(_)) => log("agent: expected initialize first; ignoring"),
            Ok(None) => {
                log("agent: stdin closed before initialize; exiting");
                return None;
            }
            Err(e) => {
                log(&format!("agent: error reading stdin ({e}); exiting"));
                return None;
            }
        }
    }
}

fn run_decision_loop<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    config: &Config,
    run: &mut RunState,
    memory: &mut Memory,
    llm_client: &llm::LlmClient,
) {
    loop {
        match protocol::read_message(reader) {
            Ok(Some(Inbound::DecisionRequest { sequence, snapshot })) => {
                let response = planner::decide(sequence, &snapshot, config, run, memory, llm_client);
                let is_finish = response.action == "finish";
                if let Err(e) = protocol::write_response(writer, &response) {
                    log(&format!("agent: failed to write response ({e}); exiting"));
                    return;
                }
                if is_finish {
                    log("agent: sent our own finish; exiting ahead of the backend's closing message");
                    return;
                }
            }
            Ok(Some(Inbound::Finish(payload))) => {
                log(&format!("agent: received finish ({payload}); exiting"));
                return;
            }
            Ok(Some(Inbound::Initialize(_))) => log("agent: received a second initialize; ignoring"),
            Ok(Some(Inbound::Unrecognized(reason))) => log(&format!("agent: ignoring unrecognized line ({reason})")),
            Ok(None) => {
                log("agent: stdin closed; exiting");
                return;
            }
            Err(e) => {
                log(&format!("agent: error reading stdin ({e}); exiting"));
                return;
            }
        }
    }
}
