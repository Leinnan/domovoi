`capabilities.json`, `keybindings.json` and `settings.json` are default values taken
from t3code (https://github.com/pingdotgg/t3code, commit f313d0a, MIT License,
Copyright (c) 2026 T3 Tools Inc.). `settings.json` is `ServerSettings` decoded from `{}`
(see `conformance/gen-settings-defaults.ts`); the others were read from a fresh server's
config snapshot.
