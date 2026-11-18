import * as Schema from "effect/Schema";
import { ServerSettings } from "../.reference/t3code/packages/contracts/src/settings.ts";
const codec = Schema.toCodecJson(ServerSettings);
const decoded = Schema.decodeUnknownSync(codec as never)({});
console.log(JSON.stringify(Schema.encodeSync(codec as never)(decoded), null, 1));
