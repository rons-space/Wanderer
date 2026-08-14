import { useEffect, useState } from "react";
import { toast } from "sonner";
import { api, errorMessage, MigrationStatus } from "@/lib/api";
import { EnableEncryption } from "../EnableEncryption";
import { TelegramLoginForm, useTelegramLogin } from "../TelegramLogin";
import { Alert, AlertDescription, AlertTitle } from "../ui/alert";
import { Button } from "../ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "../ui/card";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Progress } from "../ui/progress";
import { TabsContent } from "../ui/tabs";
import type { SecurityStatus } from "./use-security-status";

interface AccountTabProps {
    securityStatus: SecurityStatus | null;
    /** Re-reads the status the whole settings view shares. */
    reloadSecurityStatus: () => Promise<void>;
}

export function AccountTab({ securityStatus, reloadSecurityStatus }: AccountTabProps) {
    const [user, setUser] = useState<string | null>(null);
    const [isLoading, setIsLoading] = useState(false);
    const [authenticated, setAuthenticated] = useState(false);
    const [encryptionFlowOpen, setEncryptionFlowOpen] = useState(false);
    const [currentPassphrase, setCurrentPassphrase] = useState("");
    const [nextPassphrase, setNextPassphrase] = useState("");
    const [nextPassphraseConfirm, setNextPassphraseConfirm] = useState("");
    const [isChangingPassphrase, setIsChangingPassphrase] = useState(false);
    const [migrationStatus, setMigrationStatus] = useState<MigrationStatus | null>(
        securityStatus?.migration ?? null,
    );
    const [isRetryingPurge, setIsRetryingPurge] = useState(false);
    const login = useTelegramLogin({
        onAuthenticated: (loggedInUser) => {
            setUser(loggedInUser);
            setAuthenticated(true);
        },
    });

    const checkAuth = async () => {
        try {
            const me = await api.getMe();
            setUser(me ?? null);
            setAuthenticated(Boolean(me));
        } catch (e) {
            console.error("Failed to load the Telegram session:", e);
            setUser(null);
            setAuthenticated(false);
        }
    };

    useEffect(() => {
        const handleAuthChange = () => checkAuth();

        checkAuth();
        window.addEventListener("auth-changed", handleAuthChange);
        return () => window.removeEventListener("auth-changed", handleAuthChange);
    }, []);

    const loadMigrationStatus = async () => {
        try {
            setMigrationStatus(await api.getEncryptionMigrationStatus());
        } catch (e) {
            console.error("Failed to load migration status:", e);
        }
    };

    // Only encrypted libraries have a migration to watch, and the worker reports
    // no events, so the panel polls while it is on screen.
    useEffect(() => {
        if (securityStatus?.securityMode !== "encrypted") return;
        loadMigrationStatus();
        const id = setInterval(() => {
            loadMigrationStatus();
        }, 3000);
        return () => clearInterval(id);
    }, [securityStatus?.securityMode]);

    const handleLogout = async () => {
        if (!confirm("Are you sure you want to disconnect? This will remove your local session file.")) return;
        setIsLoading(true);
        try {
            await api.logout();
            setUser(null);
            setAuthenticated(false);
            toast.success("Disconnected successfully");
            window.dispatchEvent(new Event("auth-changed"));
        } catch (e) {
            console.error(e);
            toast.error("Failed to disconnect");
        } finally {
            setIsLoading(false);
        }
    };

    const handleChangePassphrase = async () => {
        if (nextPassphrase !== nextPassphraseConfirm) {
            toast.error("New passphrase confirmation does not match");
            return;
        }

        setIsChangingPassphrase(true);
        try {
            await api.changePassphrase(currentPassphrase, nextPassphrase);
            // The backend enforces the length rule and the current-passphrase
            // check, so there is nothing to validate here beyond the two new
            // fields agreeing with each other.
            setCurrentPassphrase("");
            setNextPassphrase("");
            setNextPassphraseConfirm("");
            toast.success("Passphrase changed");
        } catch (e) {
            toast.error(`Failed to change passphrase: ${errorMessage(e)}`);
        } finally {
            setIsChangingPassphrase(false);
        }
    };

    /** The user has saved and confirmed the key, so the panel can close. */
    const handleEncryptionFlowFinished = async () => {
        setEncryptionFlowOpen(false);
        await reloadSecurityStatus();
    };

    const handleEncryptionEnabled = async () => {
        // Keeps the panel mounted: the card is otherwise hidden the moment the
        // mode reads back as encrypted, which would take the one-time recovery
        // key off the screen before the user had saved it.
        setEncryptionFlowOpen(true);
        try {
            await api.startEncryptionMigration();
            toast.info("Started background migration of existing uploaded media.");
            await loadMigrationStatus();
        } catch (e) {
            console.warn("Migration start failed:", e);
        }
    };

    return (
        <TabsContent value="account" className="mt-6 space-y-4">
            <Card>
                <CardHeader>
                    <CardTitle>Telegram Account</CardTitle>
                    <CardDescription>
                        {authenticated
                            ? "Your account is connected to Telegram"
                            : "Connect your Telegram account to backup photos"}
                    </CardDescription>
                </CardHeader>
                <CardContent>
                    {authenticated ? (
                        <div className="space-y-4">
                            <div className="space-y-2">
                                <Label>Logged in as</Label>
                                <div className="bg-muted p-3 rounded-md font-mono">{user}</div>
                            </div>
                            <Alert>
                                <AlertTitle>Telegram Connected</AlertTitle>
                                <AlertDescription>Your photos are being backed up to Saved Messages.</AlertDescription>
                            </Alert>
                            <Button
                                variant="destructive"
                                className="w-full mt-4"
                                onClick={handleLogout}
                                disabled={isLoading}
                            >
                                Disconnect Account
                            </Button>
                        </div>
                    ) : (
                        <TelegramLoginForm
                            login={login}
                            idPrefix="settings"
                            banner={
                                login.error ? (
                                    <div className="bg-destructive/10 text-destructive rounded p-3 text-sm">
                                        {login.error}
                                    </div>
                                ) : null
                            }
                        />
                    )}
                </CardContent>
                {authenticated && (
                    <CardFooter>
                        <Button variant="outline" className="w-full" disabled>
                            Log Out (Not Implemented)
                        </Button>
                    </CardFooter>
                )}
            </Card>

            <Card>
                <CardHeader>
                    <CardTitle>Security</CardTitle>
                    <CardDescription>
                        Encryption mode and recovery settings for this device.
                    </CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                    <div className="flex items-center justify-between">
                        <Label>Current Mode</Label>
                        <span className="text-sm font-medium">
                            {securityStatus?.securityMode === "encrypted"
                                ? "Encrypted"
                                : securityStatus?.securityMode === "unencrypted"
                                    ? "Unencrypted"
                                    : "Not configured"}
                        </span>
                    </div>

                    {securityStatus?.securityMode === "encrypted" && (
                        <Alert>
                            <AlertTitle>Encryption Enabled</AlertTitle>
                            <AlertDescription>
                                This mode is one-way. To avoid privacy regressions, disabling encryption is not available.
                            </AlertDescription>
                        </Alert>
                    )}

                    {securityStatus?.securityMode === "encrypted" && (
                        <div className="space-y-3 rounded-md border p-3">
                            <div>
                                <Label>Change Passphrase</Label>
                                <p className="text-muted-foreground text-xs">
                                    The master key is unchanged, so nothing already encrypted is
                                    rewritten and your recovery key keeps working.
                                </p>
                            </div>
                            <div className="space-y-2">
                                <Label htmlFor="current-passphrase">Current Passphrase</Label>
                                <Input
                                    id="current-passphrase"
                                    type="password"
                                    autoComplete="current-password"
                                    value={currentPassphrase}
                                    onChange={(e) => setCurrentPassphrase(e.target.value)}
                                />
                            </div>
                            <div className="space-y-2">
                                <Label htmlFor="next-passphrase">New Passphrase</Label>
                                <Input
                                    id="next-passphrase"
                                    type="password"
                                    autoComplete="new-password"
                                    value={nextPassphrase}
                                    onChange={(e) => setNextPassphrase(e.target.value)}
                                    placeholder="At least 8 characters"
                                />
                            </div>
                            <div className="space-y-2">
                                <Label htmlFor="next-passphrase-confirm">Confirm New Passphrase</Label>
                                <Input
                                    id="next-passphrase-confirm"
                                    type="password"
                                    autoComplete="new-password"
                                    value={nextPassphraseConfirm}
                                    onChange={(e) => setNextPassphraseConfirm(e.target.value)}
                                />
                            </div>
                            <Button
                                className="w-full"
                                onClick={handleChangePassphrase}
                                disabled={isChangingPassphrase}
                            >
                                Change Passphrase
                            </Button>
                        </div>
                    )}

                    {securityStatus?.securityMode === "encrypted" && (
                        <div className="space-y-3 rounded-md border p-3">
                            <div className="flex items-center justify-between">
                                <Label>Migration Progress</Label>
                                <span className="text-xs text-muted-foreground">
                                    {(migrationStatus?.running ?? false) ? "Running" : "Idle"}
                                </span>
                            </div>
                            <Progress
                                value={
                                    (migrationStatus?.total ?? 0) > 0
                                        ? Math.min(
                                            100,
                                            ((migrationStatus?.processed ?? 0) / (migrationStatus?.total ?? 1)) * 100
                                        )
                                        : 0
                                }
                            />
                            <div className="grid grid-cols-2 gap-2 text-xs text-muted-foreground">
                                <p>Total: {migrationStatus?.total ?? 0}</p>
                                <p>Processed: {migrationStatus?.processed ?? 0}</p>
                                <p>Succeeded: {migrationStatus?.succeeded ?? 0}</p>
                                <p>Failed: {migrationStatus?.failed ?? 0}</p>
                            </div>
                            {(migrationStatus?.unpurgedPlaintext?.length ?? 0) > 0 && (
                                <Alert variant="destructive">
                                    <AlertTitle>Unencrypted copies still in Telegram</AlertTitle>
                                    <AlertDescription className="space-y-2 text-xs">
                                        <p>
                                            {migrationStatus?.unpurgedPlaintext.length} item(s) were
                                            re-uploaded encrypted, but their original unencrypted copy
                                            could not be confirmed deleted from Telegram.
                                        </p>
                                        <Button
                                            variant="outline"
                                            size="sm"
                                            disabled={isRetryingPurge}
                                            onClick={async () => {
                                                setIsRetryingPurge(true);
                                                try {
                                                    const purged = await api.retryPlaintextPurge();
                                                    toast.success(
                                                        `Deleted ${purged} unencrypted copies`
                                                    );
                                                } catch (e) {
                                                    toast.error(`Purge retry failed: ${errorMessage(e)}`);
                                                } finally {
                                                    setIsRetryingPurge(false);
                                                    await loadMigrationStatus();
                                                }
                                            }}
                                        >
                                            {isRetryingPurge ? "Deleting..." : "Retry deletion"}
                                        </Button>
                                    </AlertDescription>
                                </Alert>
                            )}
                            {migrationStatus?.lastError && (
                                <Alert>
                                    <AlertTitle>Last Migration Error</AlertTitle>
                                    <AlertDescription className="text-xs break-all">
                                        {migrationStatus.lastError}
                                    </AlertDescription>
                                </Alert>
                            )}
                            <div className="flex gap-2">
                                <Button
                                    variant="outline"
                                    className="w-full"
                                    disabled={migrationStatus?.running}
                                    onClick={async () => {
                                        try {
                                            await api.startEncryptionMigration();
                                            toast.success("Migration started");
                                            await loadMigrationStatus();
                                        } catch (e) {
                                            toast.error(`Failed to start migration: ${errorMessage(e)}`);
                                        }
                                    }}
                                >
                                    Resume Migration
                                </Button>
                                <Button
                                    variant="outline"
                                    className="w-full"
                                    onClick={loadMigrationStatus}
                                >
                                    Refresh Status
                                </Button>
                            </div>
                        </div>
                    )}

                    {(securityStatus?.securityMode !== "encrypted" || encryptionFlowOpen) && (
                        <div className="space-y-3 rounded-md border p-3">
                            <EnableEncryption
                                onEnabled={handleEncryptionEnabled}
                                onComplete={handleEncryptionFlowFinished}
                                continueLabel="Done"
                            />
                        </div>
                    )}
                </CardContent>
            </Card>

        </TabsContent>
    );
}
