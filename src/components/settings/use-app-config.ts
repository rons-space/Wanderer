import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { api } from "@/lib/api";

/**
 * The settings the backend will accept a write for. The values are strings
 * because that is how the config table stores them; the backend validates each
 * one against its own allowlist, so a typo here is refused rather than saved.
 */
export interface AppConfig {
    cache_size_mb: string;
    view_cache_max_size_mb: string;
    view_cache_retention_hours: string;
    ai_face_enabled: string;
    ai_tags_enabled: string;
    timeline_grouping: string; // 'day' | 'month' | 'year'
}

export const DEFAULT_CONFIG: AppConfig = {
    cache_size_mb: "5000",
    view_cache_max_size_mb: "2000",
    view_cache_retention_hours: "24",
    ai_face_enabled: "false",
    ai_tags_enabled: "false",
    timeline_grouping: "day",
};

export interface AppConfigState {
    config: AppConfig;
    isSaving: boolean;
    saveConfig: (key: keyof AppConfig, value: string) => Promise<void>;
    /**
     * Updates the displayed value without writing it. The sliders call this
     * while they are being dragged and `saveConfig` once on release, so a drag
     * across the range is one write rather than one per pixel.
     */
    previewConfig: (key: keyof AppConfig, value: string) => void;
}

/**
 * The config three of the five tabs read and write. It lives above them rather
 * than in each one so that a value saved on one tab is what the next tab shows.
 */
export function useAppConfig(): AppConfigState {
    const [config, setConfig] = useState<AppConfig>(DEFAULT_CONFIG);
    const [isSaving, setIsSaving] = useState(false);

    useEffect(() => {
        const load = async () => {
            try {
                const data = await api.getAllConfig();
                setConfig({
                    cache_size_mb: data.cache_size_mb || DEFAULT_CONFIG.cache_size_mb,
                    view_cache_max_size_mb:
                        data.view_cache_max_size_mb || DEFAULT_CONFIG.view_cache_max_size_mb,
                    view_cache_retention_hours:
                        data.view_cache_retention_hours || DEFAULT_CONFIG.view_cache_retention_hours,
                    ai_face_enabled: data.ai_face_enabled || DEFAULT_CONFIG.ai_face_enabled,
                    ai_tags_enabled: data.ai_tags_enabled || DEFAULT_CONFIG.ai_tags_enabled,
                    timeline_grouping: data.timeline_grouping || DEFAULT_CONFIG.timeline_grouping,
                });
            } catch (e) {
                console.error("Failed to load config:", e);
            }
        };
        load();
    }, []);

    const saveConfig = useCallback(async (key: keyof AppConfig, value: string) => {
        setIsSaving(true);
        try {
            await api.setConfig(key, value);
            setConfig((previous) => ({ ...previous, [key]: value }));
            toast.success("Settings saved");
        } catch (e) {
            console.error("Failed to save setting:", e);
            toast.error("Failed to save setting");
        } finally {
            setIsSaving(false);
        }
    }, []);

    const previewConfig = useCallback((key: keyof AppConfig, value: string) => {
        setConfig((previous) => ({ ...previous, [key]: value }));
    }, []);

    return { config, isSaving, saveConfig, previewConfig };
}
