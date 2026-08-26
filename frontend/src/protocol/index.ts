/**
 * Webland protocol, browser side.
 *
 * Only the transport seam is declared here. WebSocket is the first
 * implementation; nothing in this type may assume it, so WebTransport
 * can be added without touching callers.
 */
export interface Transport {
  send(frame: ArrayBufferView | ArrayBuffer): void;
  onMessage(handler: (frame: ArrayBuffer) => void): void;
  close(): void;
}

/** Negotiated at connect time. Must match `webland-protocol::VERSION`. */
export const VERSION = 0;
