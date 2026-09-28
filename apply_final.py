# ============================================================
# 1. crates/lib/src/query/mod.rs
# ============================================================
path = 'crates/lib/src/query/mod.rs'
with open(path, 'r') as f:
    content = f.read()

# 1a. Add debug_enabled to QueryEngineConfig
old = "    pub agent_kind: AgentKind,\n}"
new = "    pub agent_kind: AgentKind,\n    /// Whether debug mode is enabled.\n    pub debug_enabled: bool,\n}"
assert content.count(old) == 1
content = content.replace(old, new)

# 1b. Add debug fields to QueryEngine struct
old = "    pending_effort: Arc<std::sync::Mutex<Option<String>>>,\n}"
new = "    pending_effort: Arc<std::sync::Mutex<Option<String>>>,\n    /// Last assistant response text received from the LLM (debug).\n    last_llm_response: std::sync::Mutex<String>,\n    /// Location/code where the agent stopped (debug).\n    stop_location: std::sync::Mutex<String>,\n}"
assert content.count(old) == 1
content = content.replace(old, new)

# 1c. Init new fields in new()
old = "            pending_effort,\n        }\n    }\n\n    /// Live plan-mode flag"
new = "            pending_effort,\n            last_llm_response: std::sync::Mutex::new(String::new()),\n            stop_location: std::sync::Mutex::new(String::new()),\n        }\n    }\n\n    /// Live plan-mode flag"
assert content.count(old) == 1
content = content.replace(old, new)

# 1d. Add debug_response_text in streaming loop
old = "            let mut error_text = String::new();"
new = "            let mut error_text = String::new();\n            /// Debug: capture the full response text received from the LLM.\n            let mut debug_response_text = String::new();"
assert content.count(old) == 1
content = content.replace(old, new)

# 1e. Capture text deltas
old = "                                Some(StreamEvent::TextDelta(text)) => {\n                                    sink.on_text(&text);\n                                }"
new = "                                Some(StreamEvent::TextDelta(text)) => {\n                                    sink.on_text(&text);\n                                    debug_response_text.push_str(&text);\n                                }"
assert content.count(old) == 1
content = content.replace(old, new)

# 1f. Debug at "no tools" turn complete
old = "                sink.on_turn_complete(turn + 1);\n                sink.on_turn_outcome(self.state.turn_count, \"done\");\n                self.state.is_query_active = false;\n\n                // PostTurn hooks fire on successful completion."
new = "                sink.on_turn_complete(turn + 1);\n                sink.on_turn_outcome(self.state.turn_count, \"done\");\n                self.state.is_query_active = false;\n\n                // Debug: print last response and stop location.\n                if self.config.debug_enabled {\n                    *self.last_llm_response_mut().lock().unwrap() = debug_response_text;\n                    *self.stop_location_mut().lock().unwrap() = format!(\n                        \"run_turn_inner: turn {} completed (no tool calls), stop_reason={:?}\",\n                        turn + 1,\n                        stop_reason,\n                    );\n                    self.print_debug_stop();\n                }\n\n                // PostTurn hooks fire on successful completion."
assert content.count(old) == 1
content = content.replace(old, new)

# 1g. Debug at max turns end + add helper methods
old = "        self.state.is_query_active = false;\n        Ok(())\n    }\n\n    /// Cancel the current operation."
new = """        self.state.is_query_active = false;

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
# 2. crates/cli/src/commands/mod.rs
# ============================================================
path = 'crates/cli/src/commands/mod.rs'
with open(path, 'r') as f:
    content = f.read()

# 2a. Add /debug to COMMANDS
old = '    Command {\n        name: "settings",'
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

# 2b. Add execute handler
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
print("commands done")

# ============================================================
# 3. crates/lib/src/config/schema.rs
# ============================================================
path = 'crates/lib/src/config/schema.rs'
with open(path, 'r') as f:
    content = f.read()

old = "    pub dump_failed_requests: bool,\n}"
new = "    pub dump_failed_requests: bool,\n    /// Print last LLM response and stop location when agent stops.\n    pub debug_mode: bool,\n}"
assert content.count(old) == 1
content = content.replace(old, new)

old = "            dump_failed_requests: true,\n        }\n    }\n}"
new = "            dump_failed_requests: true,\n            debug_mode: false,\n        }\n    }\n}"
assert content.count(old) == 1
content = content.replace(old, new)

with open(path, 'w') as f:
    f.write(content)
print("schema done")

# ============================================================
# 4. crates/cli/src/main.rs
# ============================================================
path = 'crates/cli/src/main.rs'
with open(path, 'r') as f:
    content = f.read()

# 4a. Add debug arg (with proper attrs)
old = "    /// Enable verbose output.\n    #[arg(short, long)]\n    verbose: bool,"
new = "    /// Enable verbose output.\n    #[arg(short, long)]\n    verbose: bool,\n    /// Enable debug mode (prints last LLM response and stop location).\n    #[arg(long)]\n    debug: bool,"
assert content.count(old) == 1
content = content.replace(old, new)

# 4b. Pass debug to QueryEngineConfig - use unique anchor
old = "            agent_kind,\n        },"
new = "            agent_kind,\n            debug_enabled: cli.debug,\n        },"
assert content.count(old) == 1
content = content.replace(old, new)

with open(path, 'w') as f:
    f.write(content)
print("main.rs done")

print("ALL DONE")
