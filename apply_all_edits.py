# ============================================================
# 1. Apply all changes to crates/lib/src/query/mod.rs
# ============================================================
path = 'crates/lib/src/query/mod.rs'
with open(path, 'r') as f:
    content = f.read()

# 1a. Add debug_enabled field to QueryEngineConfig
old = """    pub agent_kind: AgentKind,
}"""
new = """    pub agent_kind: AgentKind,
    /// Whether debug mode is enabled. When enabled, the agent
    /// prints its last received response and the stop location
    /// when it stops sending requests to the LLM.
    pub debug_enabled: bool,
}"""
assert content.count(old) == 1
content = content.replace(old, new)

# 1b. Add debug fields to QueryEngine struct
old = """    pending_model: Arc<std::sync::Mutex<Option<String>>>,
    pending_effort: Arc<std::sync::Mutex<Option<String>>>,
}"""
new = """    pending_model: Arc<std::sync::Mutex<Option<String>>>,
    pending_effort: Arc<std::sync::Mutex<Option<String>>>,
    /// Last assistant response text received from the LLM (debug).
    last_llm_response: std::sync::Mutex<String>,
    /// Location/code where the agent stopped (debug).
    stop_location: std::sync::Mutex<String>,
}"""
assert content.count(old) == 1
content = content.replace(old, new)

# 1c. Initialize new fields in QueryEngine::new
old = """            pending_model,
            pending_effort,
        }
    }

    /// Live plan-mode flag"""
new = """            pending_model,
            pending_effort,
            last_llm_response: std::sync::Mutex::new(String::new()),
            stop_location: std::sync::Mutex::new(String::new()),
        }
    }

    /// Live plan-mode flag"""
assert content.count(old) == 1
content = content.replace(old, new)

# 1d. Add debug_response_text variable in streaming loop
old = """            let mut content_blocks = Vec::new();
            let mut usage = Usage::default();
            let mut stop_reason: Option<StopReason> = None;
            let mut got_error = false;
            let mut error_text = String::new();"""
new = """            let mut content_blocks = Vec::new();
            let mut usage = Usage::default();
            let mut stop_reason: Option<StopReason> = None;
            let mut got_error = false;
            let mut error_text = String::new();
            /// Debug: capture the full response text received from the LLM.
            let mut debug_response_text = String::new();"""
assert content.count(old) == 1
content = content.replace(old, new)

# 1e. Capture text deltas for debug
old = """                Some(StreamEvent::TextDelta(text)) => {
                                    sink.on_text(&text);
                                }"""
new = """                Some(StreamEvent::TextDelta(text)) => {
                                    sink.on_text(&text);
                                    debug_response_text.push_str(&text);
                                }"""
assert content.count(old) == 1
content = content.replace(old, new)

# 1f. Add debug print at "no tools" turn complete
old = """                // No tools requested — turn is complete.
                info!("Turn complete (no tool calls)");
                sink.on_turn_complete(turn + 1);
                sink.on_turn_outcome(self.state.turn_count, "done");
                self.state.is_query_active = false;"""
new = """                // No tools requested — turn is complete.
                info!("Turn complete (no tool calls)");
                sink.on_turn_complete(turn + 1);
                sink.on_turn_outcome(self.state.turn_count, "done");
                self.state.is_query_active = false;

                // Debug: print last response and stop location.
                if self.config.debug_enabled {
                    *self.last_llm_response_mut().lock().unwrap() = debug_response_text;
                    *self.stop_location_mut().lock().unwrap() = format!(
                        "run_turn_inner: turn {} completed (no tool calls), stop_reason={:?}",
                        turn + 1,
                        stop_reason,
                    );
                    self.print_debug_stop();
                }"""
assert content.count(old) == 1
content = content.replace(old, new)

# 1g. Add debug print at max turns
old = """        warn!("Max turns ({max_turns}) reached");
        sink.on_warning(&format!("Agent stopped after {max_turns} turns"));
        sink.on_turn_outcome(self.state.turn_count, "max_turns");
        self.state.is_query_active = false;
        Ok(())
    }

    /// Cancel the current operation."""
new = """        warn!("Max turns ({max_turns}) reached");
        sink.on_warning(&format!("Agent stopped after {max_turns} turns"));
        sink.on_turn_outcome(self.state.turn_count, "max_turns");
        self.state.is_query_active = false;

        // Debug: print last response and stop location.
        if self.config.debug_enabled {
            *self.last_llm_response_mut().lock().unwrap() = debug_response_text;
            *self.stop_location_mut().lock().unwrap() = format!(
                "run_turn_inner: max turns ({max_turns}) reached at turn {}",
                self.state.turn_count,
            );
            self.print_debug_stop();
        }
        Ok(())
    }

    /// Print debug info when agent stops sending requests.
    fn print_debug_stop(&self) {
        let response = self.last_llm_response.lock().unwrap();
        let location = self.stop_location.lock().unwrap();
        eprintln!("[DEBUG] Agent stopped sending next request to LLM:");
        eprintln!("[DEBUG] Stop location: {}", location);
        if response.is_empty() {
            eprintln!("[DEBUG] Last response: (empty)");
        } else {
            let plen = 2000usize;
            let display = if response.len() > plen {
                format!("{}... [total {} chars]", &response[..plen], response.len())
            } else {
                response.clone()
            };
            eprintln!("[DEBUG] Last response:\\n{}", display);
        }
        eprintln!("[DEBUG] End debug output");
    }

    /// Get the last LLM response for debugging.
    pub fn last_llm_response(&self) -> String {
        self.last_llm_response.lock().unwrap().clone()
    }

    /// Get the stop location for debugging.
    pub fn stop_location(&self) -> String {
        self.stop_location.lock().unwrap().clone()
    }

    /// Cancel the current operation."""
assert content.count(old) == 1
content = content.replace(old, new)

with open(path, 'w') as f:
    f.write(content)
print("mod.rs done")

# ============================================================
# 2. Apply changes to crates/cli/src/commands/mod.rs
# ============================================================
path = 'crates/cli/src/commands/mod.rs'
with open(path, 'r') as f:
    content = f.read()

# 2a. Add /debug to COMMANDS list
old = """    Command {
        name: "settings","""
new = """    Command {
        name: "debug",
        aliases: &[],
        description: "Toggle debug mode (prints last LLM response and stop location when agent stops)",
        hidden: false,
    },
    Command {
        name: "settings","""
assert content.count(old) == 1
content = content.replace(old, new)

# 2b. Add execute() handler for "debug" before settings handler
old = """        Some("settings") => {
            execute_settings(args, engine);
            CommandResult::Handled
        }"""
new = """        Some("debug") => {
            let current = engine.state().config.features.debug_mode;
            engine.state_mut().config.features.debug_mode = !current;
            let on = !current;
            println!("Debug mode {}", if on { "ON" } else { "OFF" });
            if on {
                println!("When on, the agent prints its last received LLM response and stop location whenever it stops sending requests to the LLM.");
            }
            CommandResult::Handled
        }
        Some("settings") => {
            execute_settings(args, engine);
            CommandResult::Handled
        }"""
assert content.count(old) == 1
content = content.replace(old, new)

with open(path, 'w') as f:
    f.write(content)
print("commands/mod.rs done")

# ============================================================
# 3. Apply changes to crates/lib/src/config/schema.rs
# ============================================================
path = 'crates/lib/src/config/schema.rs'
with open(path, 'r') as f:
    content = f.read()

# 3a. Add debug_mode to FeaturesConfig
old = """    /// Dump the LLM request payload to disk when a call fails, so the
    /// offending history can be inspected. Each failure overwrites the
    /// previous dump file (so storage stays bounded).
    pub dump_failed_requests: bool,
}"""
new = """    /// Dump the LLM request payload to disk when a call fails, so the
    /// offending history can be inspected. Each failure overwrites the
    /// previous dump file (so storage stays bounded).
    pub dump_failed_requests: bool,
    /// Print last LLM response and stop location when agent stops.
    pub debug_mode: bool,
}"""
assert content.count(old) == 1
content = content.replace(old, new)

# 3b. Add to Default impl
old = """            dump_failed_requests: true,
        }
    }
}"""
new = """            dump_failed_requests: true,
            debug_mode: false,
        }
    }
}"""
assert content.count(old) == 1
content = content.replace(old, new)

with open(path, 'w') as f:
    f.write(content)
print("schema.rs done")

# ============================================================
# 4. Apply changes to crates/cli/src/main.rs
# ============================================================
path = 'crates/cli/src/main.rs'
with open(path, 'r') as f:
    content = f.read()

# 4a. Add debug flag to CliArgs if not present
old = """    /// Enable verbose output.
    verbose: bool,"""
new = """    /// Enable verbose output.
    verbose: bool,
    /// Enable debug mode (prints last LLM response and stop location).
    debug: bool,"""
assert content.count(old) == 1
content = content.replace(old, new)

# 4b. Pass debug to QueryEngineConfig
old = """            agent_code_lib::query::QueryEngineConfig {
                max_turns: cli.max_turns,
                verbose: cli.verbose,
                unattended: cli.prompt.is_some(),
                agent_kind,
            },"""
new = """            agent_code_lib::query::QueryEngineConfig {
                max_turns: cli.max_turns,
                verbose: cli.verbose,
                unattended: cli.prompt.is_some(),
                agent_kind,
                debug_enabled: cli.debug,
            },"""
assert content.count(old) == 1
content = content.replace(old, new)

with open(path, 'w') as f:
    f.write(content)
print("main.rs done")

print("ALL DONE")
