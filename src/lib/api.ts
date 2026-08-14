import { invoke } from "@tauri-apps/api/core";
import { MediaItem, Album, QueueItem, Face, QueueCounts, SearchFilters, Tag, Person } from "../types";

/**
 * Stable classification of a backend failure, mirroring `ErrorCode` in errors.rs.
 *
 * Branch on these rather than on the message: the message is written for a person and
 * gets reworded, and for failures that came out of a library it is deliberately vague,
 * because the real cause names database columns and absolute paths and stays in the log.
 */
export type ErrorCode =
    | "databaseNotInitialized"
    | "vaultLocked"
    | "notFound"
    | "invalidInput"
    | "unavailable"
    | "database"
    | "io"
    | "telegram"
    | "internal";

export interface AppError {
    code: ErrorCode;
    message: string;
}

/** Tauri rejects with whatever the command serialized, so this narrows an unknown. */
export function asAppError(error: unknown): AppError | null {
    if (
        typeof error === "object" &&
        error !== null &&
        "code" in error &&
        "message" in error &&
        typeof (error as { code: unknown }).code === "string" &&
        typeof (error as { message: unknown }).message === "string"
    ) {
        return error as AppError;
    }
    return null;
}

/** The message to show for a rejection, whatever shape it arrived in. */
export function errorMessage(error: unknown): string {
    const raw =
        asAppError(error)?.message ?? (error instanceof Error ? error.message : String(error));

    // A rejection that never reached the AppError shape, a panic in a command or
    // a throw from the webview side, stringifies with an "Error:" prefix. Inside
    // a toast that already begins "Failed to save credentials" that prefix is
    // noise, so it comes off here rather than at each of the call sites.
    return raw.startsWith("Error:") ? raw.slice(6).trim() : raw;
}

export function hasErrorCode(error: unknown, code: ErrorCode): boolean {
    return asAppError(error)?.code === code;
}

export interface MigrationStatus {
    running: boolean;
    total: number;
    processed: number;
    succeeded: number;
    failed: number;
    lastError?: string | null;
    /** Telegram message IDs whose unencrypted copy is still in the cloud. */
    unpurgedPlaintext: number[];
}

export const api = {
    /**
     * Sends a webview crash to the backend log.
     *
     * Deliberately swallows its own failure: this is called from an error
     * boundary and from the global handlers, and a throw here would either
     * replace the crash being reported or start a loop of reports about
     * failing to report.
     */
    reportFrontendError: async (context: string, message: string, stack?: string): Promise<void> => {
        try {
            await invoke("report_frontend_error", { context, message, stack });
        } catch (e) {
            console.error("Failed to report a frontend error:", e);
        }
    },

    /** Absolute path of the log file, for telling the user what to attach. */
    getLogPath: async (): Promise<string | null> => {
        return await invoke("get_log_path");
    },

    getSecurityStatus: async (): Promise<{
        onboardingComplete: boolean;
        securityMode: string;
        encryptionConfigured: boolean;
        encryptionLocked: boolean;
        telegramCredentialsConfigured: boolean;
        migration: MigrationStatus;
    }> => {
        return await invoke("get_security_status");
    },

    initializeUnencryptedMode: async (): Promise<void> => {
        return await invoke("initialize_unencrypted_mode");
    },

    initializeEncryption: async (passphrase: string): Promise<{ recoveryKey: string }> => {
        return await invoke("initialize_encryption", { passphrase });
    },

    unlockEncryption: async (passphrase: string): Promise<void> => {
        return await invoke("unlock_encryption", { passphrase });
    },

    lockEncryption: async (): Promise<void> => {
        return await invoke("lock_encryption");
    },

    /**
     * Reset the passphrase with the recovery key.
     *
     * Returns a *new* recovery key: the one just used is retired, because it
     * has been typed into a machine and came from wherever the user was keeping
     * it. Callers must show what comes back, since nothing can produce it again.
     */
    recoverEncryption: async (
        recoveryKey: string,
        newPassphrase: string,
    ): Promise<{ recoveryKey: string }> => {
        return await invoke("recover_encryption", { recoveryKey, newPassphrase });
    },

    /** Change the passphrase for someone who still knows the current one. */
    changePassphrase: async (currentPassphrase: string, newPassphrase: string): Promise<void> => {
        return await invoke("change_passphrase", { currentPassphrase, newPassphrase });
    },

    regenerateRecoveryKey: async (passphrase: string): Promise<{ recoveryKey: string }> => {
        return await invoke("regenerate_recovery_key", { passphrase });
    },

    completeOnboarding: async (): Promise<void> => {
        return await invoke("complete_onboarding");
    },

    setTelegramApiCredentials: async (apiId: number, apiHash: string): Promise<void> => {
        return await invoke("set_telegram_api_credentials", { apiId, apiHash });
    },

    clearTelegramApiCredentials: async (): Promise<void> => {
        return await invoke("clear_telegram_api_credentials");
    },

    startEncryptionMigration: async (): Promise<void> => {
        return await invoke("start_encryption_migration");
    },

    getEncryptionMigrationStatus: async (): Promise<MigrationStatus> => {
        return await invoke("get_encryption_migration_status");
    },

    /**
     * Retry deleting plaintext copies the migration left in Telegram.
     * Resolves with the number confirmed deleted by this attempt.
     */
    retryPlaintextPurge: async (): Promise<number> => {
        return await invoke("retry_plaintext_purge");
    },

    getMe: async (): Promise<string> => {
        return await invoke("get_me");
    },

    getMedia: async (limit: number, offset: number): Promise<MediaItem[]> => {
        return await invoke("get_media", { limit, offset });
    },

    searchMedia: async (query: string, limit: number, offset: number): Promise<MediaItem[]> => {
        return await invoke("search_media", { query, limit, offset });
    },

    searchFts: async (query: string, filters: SearchFilters, limit: number, offset: number): Promise<MediaItem[]> => {
        return await invoke("search_fts", { query, filters, limit, offset });
    },

    createAlbum: async (name: string): Promise<number> => {
        return await invoke("create_album", { name });
    },

    getAlbums: async (): Promise<Album[]> => {
        return await invoke("get_albums");
    },

    addMediaToAlbum: async (albumId: number, mediaId: number): Promise<void> => {
        return await invoke("add_media_to_album", { albumId, mediaId });
    },

    getAlbumMedia: async (albumId: number, limit: number, offset: number): Promise<MediaItem[]> => {
        return await invoke("get_album_media", { albumId, limit, offset });
    },

    loginRequestCode: async (phone: string): Promise<void> => {
        return await invoke("login_request_code", { phone });
    },

    loginSignIn: async (code: string): Promise<string> => {
        return await invoke("login_sign_in", { code });
    },

    logout: async (): Promise<void> => {
        return await invoke("logout");
    },

    importFiles: async (files: string[]): Promise<number> => {
        return await invoke("import_files", { files });
    },

    getQueueStatus: async (): Promise<QueueItem[]> => {
        return await invoke("get_queue_status");
    },

    detectFaces: async (path: string): Promise<Face[]> => {
        return await invoke("detect_faces", { path });
    },

    getFaces: async (mediaId: number): Promise<Face[]> => {
        return await invoke("get_faces", { mediaId });
    },

    // Phase 2: Favorites & Ratings
    toggleFavorite: async (mediaId: number): Promise<boolean> => {
        return await invoke("toggle_favorite", { mediaId });
    },

    setRating: async (mediaId: number, rating: number): Promise<void> => {
        return await invoke("set_rating", { mediaId, rating });
    },

    getFavorites: async (limit: number, offset: number): Promise<MediaItem[]> => {
        return await invoke("get_favorites", { limit, offset });
    },

    // Phase 2: Trash
    softDeleteMedia: async (mediaId: number): Promise<void> => {
        return await invoke("soft_delete_media", { mediaId });
    },

    restoreFromTrash: async (mediaId: number): Promise<void> => {
        return await invoke("restore_from_trash", { mediaId });
    },

    getTrash: async (limit: number, offset: number): Promise<MediaItem[]> => {
        return await invoke("get_trash", { limit, offset });
    },

    // Phase 3: Upload Queue
    getUploadQueue: async (): Promise<QueueItem[]> => {
        return await invoke("get_upload_queue");
    },

    getQueueCounts: async (): Promise<QueueCounts> => {
        return await invoke("get_queue_counts");
    },

    retryUpload: async (id: number): Promise<void> => {
        return await invoke("retry_upload", { id });
    },

    // Phase 5: Bulk Operations
    bulkSetFavorite: async (mediaIds: number[], isFavorite: boolean): Promise<number> => {
        return await invoke("bulk_set_favorite", { mediaIds, isFavorite });
    },

    bulkDelete: async (mediaIds: number[]): Promise<number> => {
        return await invoke("bulk_delete", { mediaIds });
    },

    bulkAddToAlbum: async (albumId: number, mediaIds: number[]): Promise<number> => {
        return await invoke("bulk_add_to_album", { albumId, mediaIds });
    },

    // Phase 6: Export & Advanced Features
    exportMedia: (mediaIds: number[], destination: string) =>
        invoke<number>("export_media", { mediaIds, destination }),
    // Phase 7: Duplicate Detection
    findDuplicates: () =>
        invoke<MediaItem[][]>("find_duplicates"),
    scanDuplicates: () =>
        invoke<number>("scan_duplicates"),
    // Phase 7: People / Face Recognition
    getPeople: () =>
        invoke<Person[]>("get_persons"),
    updatePersonName: (personId: number, name: string) =>
        invoke<void>("update_person_name", { personId, name }),
    getMediaByPerson: (personId: number, limit: number, offset: number) =>
        invoke<MediaItem[]>("get_media_by_person", { personId, limit, offset }),
    mergePersons: (targetId: number, sourceIds: number[]) =>
        invoke<void>("merge_persons", { targetId, sourceIds }),
    // Phase 7: Tags / Object Detection  
    getAllTags: () =>
        invoke<Tag[]>("get_all_tags"),
    getMediaByTag: (tag: string, limit: number, offset: number) =>
        invoke<MediaItem[]>("get_media_by_tag", { tag, limit, offset }),
    getTagsForMedia: (mediaId: number) =>
        invoke<string[]>("get_tags_for_media", { mediaId }),
    // Config / Settings
    // Keys prefixed `security_` are filtered out by the backend on both paths:
    // they hold the wrapped master key and the DPAPI credential blob, and are
    // managed by the dedicated security commands instead.
    getAllConfig: () =>
        invoke<Record<string, string>>("get_all_config"),
    setConfig: (key: string, value: string) =>
        invoke<void>("set_config", { key, value }),
    // Smart Albums
    getSmartAlbumCounts: () =>
        invoke<{ videos: number; recent: number; top_rated: number }>("get_smart_album_counts"),
    getVideos: (limit: number, offset: number) =>
        invoke<MediaItem[]>("get_videos", { limit, offset }),
    getRecent: (limit: number, offset: number) =>
        invoke<MediaItem[]>("get_recent", { limit, offset }),
    getTopRated: (limit: number, offset: number) =>
        invoke<MediaItem[]>("get_top_rated", { limit, offset }),
    // Archive
    archiveMedia: (mediaId: number) =>
        invoke<void>("archive_media", { mediaId }),
    unarchiveMedia: (mediaId: number) =>
        invoke<void>("unarchive_media", { mediaId }),
    getArchivedMedia: (limit: number, offset: number) =>
        invoke<MediaItem[]>("get_archived_media", { limit, offset }),
    // Permanent Delete
    permanentDeleteMedia: (mediaId: number, deleteFromTelegram: boolean) =>
        invoke<void>("permanent_delete_media", { mediaId, deleteFromTelegram }),
    emptyTrash: (deleteFromTelegram: boolean) =>
        invoke<number>("empty_trash", { deleteFromTelegram }),
    // Backup
    getBackupPath: () =>
        invoke<string>("get_backup_path"),
    backupDatabase: (destination?: string, uploadToTelegram?: boolean) =>
        invoke<string>("backup_database", { destination, uploadToTelegram: uploadToTelegram ?? false }),
    // Cloud-Only Mode
    removeLocalCopy: (mediaId: number) =>
        invoke<void>("remove_local_copy", { mediaId }),
    downloadLocalCopy: (mediaId: number) =>
        invoke<string>("download_local_copy", { mediaId }),
    downloadForView: (mediaId: number) =>
        invoke<string>("download_for_view", { mediaId }),
    // Share Links
    generateShareLink: (mediaId: number) =>
        invoke<string>("generate_share_link", { mediaId }),
    // Multi-Device Sync
    exportSyncManifest: () =>
        invoke<string>("export_sync_manifest"),
    importSyncManifest: (path: string) =>
        invoke<string>("import_sync_manifest", { path }),
    getDeviceId: () =>
        invoke<string>("get_device_id"),
    // CLIP Semantic Search
    checkClipModels: () =>
        invoke<boolean>("check_clip_models"),
    downloadClipModels: () =>
        invoke<void>("download_clip_models"),
    semanticSearch: (query: string, limit: number) =>
        invoke<MediaItem[]>("semantic_search", { query, limit }),
    indexPendingClip: (limit: number) =>
        invoke<number>("index_pending_clip", { limit }),
};
