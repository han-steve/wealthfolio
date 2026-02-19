// File Dialogs
import { open, save } from "@tauri-apps/plugin-dialog";
import { writeFile } from "@tauri-apps/plugin-fs";

export const openCsvFileDialog = async (): Promise<null | string | string[]> => {
  return open({ filters: [{ name: "CSV", extensions: ["csv"] }] });
};

export const openFolderDialog = async (): Promise<string | null> => {
  return open({ directory: true });
};

export const openDatabaseFileDialog = async (): Promise<string | null> => {
  const result = (await open()) as string | string[] | null;
  if (Array.isArray(result)) {
    return result[0] ?? null;
  }
  return typeof result === "string" ? result : null;
};

export const openFileSaveDialog = async (
  fileContent: string | Blob | Uint8Array,
  fileName: string,
): Promise<boolean> => {
  const filePath = await save({
    defaultPath: fileName,
    filters: [
      {
        name: fileName,
        extensions: [fileName.split(".").pop() ?? ""],
      },
    ],
  });

  if (filePath === null) {
    return false;
  }

  let contentToSave: Uint8Array;
  if (typeof fileContent === "string") {
    contentToSave = new TextEncoder().encode(fileContent);
  } else if (fileContent instanceof Blob) {
    const arrayBuffer = await fileContent.arrayBuffer();
    contentToSave = new Uint8Array(arrayBuffer);
  } else {
    contentToSave = fileContent;
  }

  // Save dialog returns an absolute path/URI; avoid forcing a baseDir, which can break mobile.
  const normalizedPath = filePath.startsWith("file://")
    ? (() => {
        try {
          return decodeURIComponent(filePath.replace(/^file:\/\//, ""));
        } catch {
          return filePath.replace(/^file:\/\//, "");
        }
      })()
    : filePath;

  await writeFile(normalizedPath, contentToSave);

  return true;
};

// ============================================================================
// Shell & Browser
// ============================================================================

export const openUrlInBrowser = async (url: string): Promise<void> => {
  const { open: openShell } = await import("@tauri-apps/plugin-shell");
  await openShell(url);
};
