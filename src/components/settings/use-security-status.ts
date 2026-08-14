import { useCallback, useEffect, useState } from "react";
import { api, MigrationStatus } from "@/lib/api";

export interface SecurityStatus {
    onboardingComplete: boolean;
    securityMode: string;
    encryptionConfigured: boolean;
    encryptionLocked: boolean;
    telegramCredentialsConfigured: boolean;
    migration: MigrationStatus;
}

export interface SecurityStatusState {
    securityStatus: SecurityStatus | null;
    reload: () => Promise<void>;
}

/**
 * Shared by the Account tab, which owns the encryption controls, and the
 * Storage tab, which describes a backup differently once the library is
 * encrypted. A failed read reads as "no status" rather than as unencrypted, so
 * neither tab claims a protection that may be in place.
 */
export function useSecurityStatus(): SecurityStatusState {
    const [securityStatus, setSecurityStatus] = useState<SecurityStatus | null>(null);

    const reload = useCallback(async () => {
        try {
            setSecurityStatus(await api.getSecurityStatus());
        } catch (e) {
            console.error("Failed to load security status:", e);
            setSecurityStatus(null);
        }
    }, []);

    useEffect(() => {
        reload();
    }, [reload]);

    return { securityStatus, reload };
}
