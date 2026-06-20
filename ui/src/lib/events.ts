import { listen } from "@tauri-apps/api/event";
import type { CoreEvent } from "../types";

export function subscribeCoreEvents(handler: (event: CoreEvent) => void) {
  const unlistenPromise = listen<CoreEvent>("core-event", (event) => {
    handler(event.payload);
  });
  return async () => {
    const unlisten = await unlistenPromise;
    unlisten();
  };
}
