import { useState } from "react";
import { api, errorMessage } from "@/lib/api";
import { TelegramLoginForm, useTelegramLogin } from "./TelegramLogin";
import { EnableEncryption, RecoveryKeyPanel } from "./EnableEncryption";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Switch } from "@/components/ui/switch";
import { Separator } from "@/components/ui/separator";
import { toast } from "sonner";
import { Shield, Cloud, LockKeyhole, Loader2, TriangleAlert, Check } from "lucide-react";

type SecurityStatus = {
    onboardingComplete: boolean;
    securityMode: string;
    encryptionConfigured: boolean;
    encryptionLocked: boolean;
    telegramCredentialsConfigured: boolean;
    migration: {
        running: boolean;
        total: number;
        processed: number;
        succeeded: number;
        failed: number;
        lastError?: string | null;
    };
};

interface OnboardingProps {
    status: SecurityStatus;
    onReady: () => Promise<void> | void;
}

type OnboardingStep = "mode" | "encrypt" | "recovery" | "byok" | "telegram" | "finish";

export function Onboarding({ status, onReady }: OnboardingProps) {
    const needsUnlockOnly =
        status.securityMode === "encrypted" && status.encryptionLocked;

    const [step, setStep] = useState<OnboardingStep>("mode");
    const login = useTelegramLogin({
        onAuthenticated: () => {
            toast.success("Telegram login successful.");
            setStep("finish");
        },
        onError: (message) => toast.error(message),
    });
    const [isBusy, setIsBusy] = useState(false);

    const [mode, setMode] = useState<"encrypted" | "unencrypted">("encrypted");
    const [acceptUnencryptedRisk, setAcceptUnencryptedRisk] = useState(false);

    const [apiId, setApiId] = useState("");
    const [apiHash, setApiHash] = useState("");


    const [unlockPassphrase, setUnlockPassphrase] = useState("");
    const [showRecoveryUnlock, setShowRecoveryUnlock] = useState(false);
    const [unlockRecoveryKey, setUnlockRecoveryKey] = useState("");
    const [resetRecoveryKey, setResetRecoveryKey] = useState<string | null>(null);
    const [unlockNewPassphrase, setUnlockNewPassphrase] = useState("");

    const withBusy = async (fn: () => Promise<void>) => {
        setIsBusy(true);
        try {
            await fn();
        } finally {
            setIsBusy(false);
        }
    };

    const handleChooseMode = async () => {
        if (mode === "unencrypted" && !acceptUnencryptedRisk) {
            toast.error("Please acknowledge the unencrypted mode warning.");
            return;
        }

        if (mode === "unencrypted") {
            try {
                await withBusy(async () => {
                    await api.initializeUnencryptedMode();
                    setStep("byok");
                });
            } catch (e) {
                toast.error(`Failed to set unencrypted mode: ${errorMessage(e)}`);
            }
            return;
        }

        if (status.encryptionConfigured || status.securityMode === "encrypted") {
            setStep("byok");
            return;
        }

        setStep("encrypt");
    };

    const handleSaveByok = async () => {
        const id = Number(apiId);
        if (!Number.isFinite(id) || id <= 0) {
            toast.error("API ID must be a positive number.");
            return;
        }
        if (!apiHash.trim()) {
            toast.error("API hash is required.");
            return;
        }

        try {
            await withBusy(async () => {
                await api.setTelegramApiCredentials(id, apiHash.trim());
                toast.success("Telegram API credentials saved.");
                setStep("telegram");
            });
        } catch (e) {
            toast.error(`Failed to save credentials: ${errorMessage(e)}`);
        }
    };

    const finalize = async () => {
        try {
            await withBusy(async () => {
                await api.completeOnboarding();
                if (mode === "encrypted") {
                    // Best effort migration kickoff for existing plaintext uploads.
                    try {
                        await api.startEncryptionMigration();
                    } catch (e) {
                        console.warn("Migration start skipped:", e);
                    }
                }
                await onReady();
            });
        } catch (e) {
            toast.error(`Failed to complete onboarding: ${errorMessage(e)}`);
        }
    };

    const handleUnlock = async () => {
        if (!unlockPassphrase.trim()) {
            toast.error("Passphrase is required.");
            return;
        }
        try {
            await withBusy(async () => {
                await api.unlockEncryption(unlockPassphrase);
                await onReady();
            });
        } catch (e) {
            toast.error(`Unlock failed: ${errorMessage(e)}`);
        }
    };

    const handleRecoveryUnlock = async () => {
        if (!unlockRecoveryKey.trim()) {
            toast.error("Recovery key is required.");
            return;
        }
        if (unlockNewPassphrase.trim().length < 8) {
            toast.error("New passphrase must be at least 8 characters.");
            return;
        }
        try {
            await withBusy(async () => {
                const { recoveryKey } = await api.recoverEncryption(
                    unlockRecoveryKey.trim(),
                    unlockNewPassphrase.trim(),
                );
                toast.success("Recovery successful. Passphrase has been reset.");
                // The key that was just used is spent, so the user has to be
                // handed the replacement before they go anywhere.
                setResetRecoveryKey(recoveryKey);
            });
        } catch (e) {
            toast.error(`Recovery failed: ${errorMessage(e)}`);
        }
    };

    if (resetRecoveryKey) {
        return (
            <div className="h-screen w-screen flex items-center justify-center bg-background p-6">
                <Card className="w-full max-w-lg">
                    <CardHeader>
                        <CardTitle>Save your new recovery key</CardTitle>
                        <CardDescription>
                            The key you just used has been retired. This one replaces it.
                        </CardDescription>
                    </CardHeader>
                    <CardContent>
                        <RecoveryKeyPanel
                            recoveryKey={resetRecoveryKey}
                            continueLabel="Continue to my library"
                            onConfirmed={() => {
                                setResetRecoveryKey(null);
                                void onReady();
                            }}
                        />
                    </CardContent>
                </Card>
            </div>
        );
    }

    if (needsUnlockOnly) {
        return (
            <div className="h-screen w-screen flex items-center justify-center bg-background p-6">
                <Card className="w-full max-w-lg">
                    <CardHeader>
                        <CardTitle className="flex items-center gap-2">
                            <LockKeyhole className="h-5 w-5" />
                            Unlock Encrypted Library
                        </CardTitle>
                        <CardDescription>
                            This library is encrypted. Enter your passphrase to continue.
                        </CardDescription>
                    </CardHeader>
                    <CardContent className="space-y-4">
                        <div className="space-y-2">
                            <Label htmlFor="unlock-passphrase">Passphrase</Label>
                            <Input
                                id="unlock-passphrase"
                                type="password"
                                value={unlockPassphrase}
                                onChange={(e) => setUnlockPassphrase(e.target.value)}
                                placeholder="Enter passphrase"
                            />
                        </div>
                        <Button className="w-full" onClick={handleUnlock} disabled={isBusy}>
                            {isBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                            Unlock
                        </Button>

                        <Separator />

                        <Button
                            variant="outline"
                            className="w-full"
                            onClick={() => setShowRecoveryUnlock((v) => !v)}
                        >
                            Use Recovery Key Instead
                        </Button>

                        {showRecoveryUnlock && (
                            <div className="space-y-3 rounded-md border p-3">
                                <div className="space-y-2">
                                    <Label htmlFor="unlock-recovery-key">Recovery Key</Label>
                                    <Input
                                        id="unlock-recovery-key"
                                        value={unlockRecoveryKey}
                                        onChange={(e) => setUnlockRecoveryKey(e.target.value)}
                                        placeholder="XXXXX-XXXXX-XXXXX..."
                                    />
                                </div>
                                <div className="space-y-2">
                                    <Label htmlFor="unlock-new-passphrase">New Passphrase</Label>
                                    <Input
                                        id="unlock-new-passphrase"
                                        type="password"
                                        value={unlockNewPassphrase}
                                        onChange={(e) => setUnlockNewPassphrase(e.target.value)}
                                        placeholder="Set a new passphrase"
                                    />
                                </div>
                                <Button
                                    className="w-full"
                                    onClick={handleRecoveryUnlock}
                                    disabled={isBusy}
                                >
                                    {isBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                                    Recover And Reset Passphrase
                                </Button>
                            </div>
                        )}
                    </CardContent>
                </Card>
            </div>
        );
    }

    return (
        <div className="h-screen w-screen flex items-center justify-center bg-background p-6">
            <Card className="w-full max-w-2xl">
                <CardHeader>
                    <CardTitle className="flex items-center gap-2">
                        <Shield className="h-5 w-5" />
                        Welcome to Wander(er)
                    </CardTitle>
                    <CardDescription>
                        Complete secure setup before using your library.
                    </CardDescription>
                </CardHeader>
                <CardContent className="space-y-6">
                    {step === "mode" && (
                        <div className="space-y-4">
                            <h3 className="font-semibold">Choose Protection Mode</h3>
                            <div className="grid gap-3 md:grid-cols-2">
                                <button
                                    className={`rounded-lg border p-4 text-left transition ${
                                        mode === "encrypted" ? "border-primary bg-primary/5" : "border-border"
                                    }`}
                                    onClick={() => setMode("encrypted")}
                                >
                                    <div className="font-semibold flex items-center gap-2">
                                        <LockKeyhole className="h-4 w-4" />
                                        Encrypted (Recommended)
                                    </div>
                                    <p className="mt-2 text-sm text-muted-foreground">
                                        Files are encrypted before Telegram upload. Cloud providers cannot view your content.
                                    </p>
                                    <p className="mt-2 text-xs text-muted-foreground">
                                        Tradeoff: if passphrase and recovery key are both lost, data is unrecoverable.
                                    </p>
                                </button>
                                <button
                                    className={`rounded-lg border p-4 text-left transition ${
                                        mode === "unencrypted" ? "border-primary bg-primary/5" : "border-border"
                                    }`}
                                    onClick={() => setMode("unencrypted")}
                                >
                                    <div className="font-semibold flex items-center gap-2">
                                        <Cloud className="h-4 w-4" />
                                        Unencrypted
                                    </div>
                                    <p className="mt-2 text-sm text-muted-foreground">
                                        Keeps current behavior. Faster setup, but cloud copy is plaintext.
                                    </p>
                                    <p className="mt-2 text-xs text-muted-foreground">
                                        Tradeoff: Telegram/cloud can access media contents.
                                    </p>
                                </button>
                            </div>

                            {mode === "unencrypted" && (
                                <Alert>
                                    <TriangleAlert className="h-4 w-4" />
                                    <AlertTitle>Unencrypted Mode Warning</AlertTitle>
                                    <AlertDescription className="space-y-3">
                                        <p>
                                            Uploads will be stored as plaintext in cloud. This weakens privacy guarantees.
                                        </p>
                                        <div className="flex items-center gap-2">
                                            <Switch
                                                checked={acceptUnencryptedRisk}
                                                onCheckedChange={setAcceptUnencryptedRisk}
                                            />
                                            <span className="text-sm">
                                                I understand the risks and still want unencrypted mode.
                                            </span>
                                        </div>
                                    </AlertDescription>
                                </Alert>
                            )}

                            <Button onClick={handleChooseMode} disabled={isBusy} className="w-full">
                                {isBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                                Continue
                            </Button>
                        </div>
                    )}

                    {(step === "encrypt" || step === "recovery") && (
                        <EnableEncryption
                            onBack={() => setStep("mode")}
                            onEnabled={() => setStep("recovery")}
                            onComplete={() => setStep("byok")}
                            continueLabel="Continue to BYOK Setup"
                        />
                    )}

                    {step === "byok" && (
                        <div className="space-y-4">
                            <h3 className="font-semibold">Bring Your Own Telegram API Credentials</h3>
                            <p className="text-sm text-muted-foreground">
                                Enter your Telegram API ID and API hash. They are stored locally using Windows DPAPI.
                            </p>
                            <Alert className="border-amber-500/60 bg-amber-500/10">
                                <TriangleAlert className="h-4 w-4 text-amber-600" />
                                <AlertTitle className="text-amber-700 dark:text-amber-300">
                                    Important: Required Before You Can Continue
                                </AlertTitle>
                                <AlertDescription className="space-y-3 text-foreground">
                                    <p className="text-sm font-medium">Quick tutorial to get your API credentials:</p>
                                    <ol className="list-decimal space-y-1 pl-5 text-sm">
                                        <li>
                                            Open{" "}
                                            <a
                                                href="https://my.telegram.org/apps"
                                                target="_blank"
                                                rel="noreferrer"
                                                className="font-medium underline underline-offset-2 hover:text-primary"
                                            >
                                                my.telegram.org/apps
                                            </a>{" "}
                                            and sign in with your Telegram account.
                                        </li>
                                        <li>
                                            Go to <span className="font-medium">API development tools</span> and create a new app
                                            (name can be anything).
                                        </li>
                                        <li>
                                            Copy the generated <span className="font-medium">api_id</span> and{" "}
                                            <span className="font-medium">api_hash</span>.
                                        </li>
                                        <li>
                                            Paste both values below, then click{" "}
                                            <span className="font-medium">Save Credentials</span>.
                                        </li>
                                    </ol>
                                    <p className="text-xs font-medium text-amber-700 dark:text-amber-300">
                                        Keep your API hash private. Do not share it publicly.
                                    </p>
                                </AlertDescription>
                            </Alert>
                            <div className="space-y-2">
                                <Label htmlFor="api-id">API ID</Label>
                                <Input
                                    id="api-id"
                                    inputMode="numeric"
                                    value={apiId}
                                    onChange={(e) => setApiId(e.target.value)}
                                    placeholder="e.g. 12345678"
                                />
                            </div>
                            <div className="space-y-2">
                                <Label htmlFor="api-hash">API Hash</Label>
                                {/*
                                    Masked: the API hash is a long-lived secret
                                    that grants access to the Telegram account,
                                    and onboarding is exactly when someone is
                                    most likely to be looking over a shoulder or
                                    screen-sharing a first run.
                                */}
                                <Input
                                    id="api-hash"
                                    type="password"
                                    autoComplete="off"
                                    value={apiHash}
                                    onChange={(e) => setApiHash(e.target.value)}
                                    placeholder="32-char API hash"
                                />
                            </div>
                            <Button className="w-full" onClick={handleSaveByok} disabled={isBusy}>
                                {isBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                                Save Credentials
                            </Button>
                        </div>
                    )}

                    {step === "telegram" && (
                        <div className="space-y-4">
                            <h3 className="font-semibold">Connect Telegram Account</h3>
                            <TelegramLoginForm login={login} idPrefix="onboarding" />
                        </div>
                    )}

                    {step === "finish" && (
                        <div className="space-y-4">
                            <Alert>
                                <Check className="h-4 w-4" />
                                <AlertTitle>Setup Complete</AlertTitle>
                                <AlertDescription>
                                    Your secure onboarding is complete. You can now access your library.
                                </AlertDescription>
                            </Alert>
                            <Button className="w-full" onClick={finalize} disabled={isBusy}>
                                {isBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                                Open Library
                            </Button>
                        </div>
                    )}
                </CardContent>
            </Card>
        </div>
    );
}
