// Writes t3code's DEFAULT_KEYBINDINGS rules (string form) for the server's
// keybindings.json seed. Usage (from .reference/t3code/apps/server):
//   node ../../../../conformance/gen-keybinding-rules.ts > ../../../../crates/domovoi-contracts/defaults/keybinding-rules.json
import { DEFAULT_KEYBINDINGS } from "../.reference/t3code/packages/shared/src/keybindings.ts";
console.log(JSON.stringify(DEFAULT_KEYBINDINGS, null, 1));
