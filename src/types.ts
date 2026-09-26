/** Mirrors `history::EntryView` on the Rust side. */
export interface ClipEntry {
  id: number;
  createdAt: number;
  kind: "text" | "image";
  /** Truncated text body, or `1920 × 1080` for images. */
  preview: string;
  truncated: boolean;
  lineCount: number;
  charCount: number;
  /** `data:image/png;base64,…`, images only. */
  thumb: string | null;
  width: number | null;
  height: number | null;
  byteLen: number;
}

/** Mirrors `PermissionStatus` on the Rust side. */
export interface PermissionStatus {
  accessibility: boolean;
  required: boolean;
}

export type Filter = "all" | "text" | "image";
