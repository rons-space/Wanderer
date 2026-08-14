import { useMemo, useState } from "react";
import { api } from "@/lib/api";
import { TelegramLoginForm, useTelegramLogin } from "./TelegramLogin";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Switch } from "@/components/ui/switch";
import { Separator } from "@/components/ui/separator";
import { toast } from "sonner";
import { Shield, KeyRound, Cloud, LockKeyhole, Loader2, TriangleAlert, Check } from "lucide-react";

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

    const [passphrase, setPassphrase] = useState("");
    const [confirmPassphrase, setConfirmPassphrase] = useState("");
    const [recoveryKey, setRecoveryKey] = useState<string | null>(null);
    const [recoverySegments, setRecoverySegments] = useState<string[]>([]);
    const verifyIndexes = useMemo(() => {
        if (recoverySegments.length < 2) return [0, 0];
        return [1, Math.max(0, recoverySegments.length - 2)];
    }, [recoverySegments.length]);
    const [verifyA, setVerifyA] = useState("");
    const [verifyB, setVerifyB] = useState("");
    const [recoveryVerified, setRecoveryVerified] = useState(false);

    const [apiId, setApiId] = useState("");
    const [apiHash, setApiHash] = useState("");


    const [unlockPassphrase, setUnlockPassphrase] = useState("");
    const [showRecoveryUnlock, setShowRecoveryUnlock] = useState(false);
    const [unlockRecoveryKey, setUnlockRecoveryKey] = useState("");
    const [unlockNewPassphrase, setUnlockNewPassphrase] = useState("");

    const withBusy = async (fn: () => Promise<void>) => {
        setIsBusy(true);
        try {
            await fn();
        } finally {
            setIsBusy(false);
        }
    };

    const toErrorMessage = (err: unknown): string => {
        const raw =
            typeof err === "string"
                ? err
                : err instanceof Error
                    ? err.message
                    : String(err);
        return raw.startsWith("Error:") ? raw.slice(6).trim() : raw;
    };

    const downloadRecoveryKey = () => {
        if (!recoveryKey) return;
        const blob = new Blob([`Wander(er) Recovery Key\n\n${recoveryKey}\n`], {
            type: "text/plain;charset=utf-8",
        });
        const link = document.createElement("a");
        link.href = URL.createObjectURL(blob);
        link.download = "wanderer-recovery-key.txt";
        link.click();
        URL.revokeObjectURL(link.href);
    };

    const printRecoveryKey = () => {
        if (!recoveryKey) return;
        const printWindow = window.open("", "_blank", "width=700,height=500");
        if (!printWindow) return;
        printWindow.document.write(
            `<pre style="font-family: ui-monospace, SFMono-Regular, Menlo, monospace; padding: 24px;">Wander(er) Recovery Key\n\n${recoveryKey}\n\nStore this securely. Anyone with this key can recover your vault.</pre>`,
        );
        printWindow.document.close();
        printWindow.focus();
        printWindow.print();
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
                toast.error(`Failed to set unencrypted mode: ${toErrorMessage(e)}`);
            }
            return;
        }

        if (status.encryptionConfigured || status.securityMode === "encrypted") {
            setStep("byok");
            return;
        }

        setStep("encrypt");
    };

    const handleInitializeEncryption = async () => {
        if (passphrase.length < 8) {
            toast.error("Passphrase must be at least 8 characters.");
            return;
        }
        if (passphrase !== confirmPassphrase) {
            toast.error("Passphrase confirmation does not match.");
            return;
        }

        try {
            await withBusy(async () => {
                const result = await api.initializeEncryption(passphrase);
                const key = result.recoveryKey.trim();
                const segments = key.split("-");
                setRecoveryKey(key);
                setRecoverySegments(segments);
                setVerifyA("");
                setVerifyB("");
                setRecoveryVerified(false);
                setStep("recovery");
            });
        } catch (e) {
            const message = toErrorMessage(e);
            toast.error(`Failed to initialize encryption: ${message}`);
            if (message.toLowerCase().includes("already enabled")) {
                setStep("byok");
            }
        }
    };

    const handleVerifyRecovery = () => {
        if (!recoverySegments.length) return;
        const expectedA = recoverySegments[verifyIndexes[0]] || "";
        const expectedB = recoverySegments[verifyIndexes[1]] || "";
        if (
            verifyA.trim().toUpperCase() !== expectedA.toUpperCase() ||
            verifyB.trim().toUpperCase() !== expectedB.toUpperCase()
        ) {
            toast.error("Recovery key verification failed.");
            return;
        }
        setRecoveryVerified(true);
        toast.success("Recovery key verified.");
    };

    const handleConfirmRecoveryStep = () => {
        if (!recoveryVerified) {
            toast.error("Verify the recovery key first.");
            return;
        }
        // Show once only in onboarding session.
        setRecoveryKey(null);
        setRecoverySegments([]);
        setStep("byok");
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
            toast.error(`Failed to save credentials: ${toErrorMessage(e)}`);
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
            toast.error(`Failed to complete onboarding: ${toErrorMessage(e)}`);
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
            toast.error(`Unlock failed: ${toErrorMessage(e)}`);
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
                await api.recoverEncryption(unlockRecoveryKey.trim(), unlockNewPassphrase.trim());
                toast.success("Recovery successful. Passphrase has been reset.");
                await onReady();
            });
        } catch (e) {
            toast.error(`Recovery failed: ${toErrorMessage(e)}`);
        }
    };

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

                    {step === "encrypt" && (
                        <div className="space-y-4">
                            <h3 className="font-semibold">Create Encryption Passphrase</h3>
                            <div className="space-y-2">
                                <Label htmlFor="passphrase">Passphrase</Label>
                                <Input
                                    id="passphrase"
                                    type="password"
                                    value={passphrase}
                                    onChange={(e) => setPassphrase(e.target.value)}
                                    placeholder="At least 8 characters"
                                />
                            </div>
                            <div className="space-y-2">
                                <Label htmlFor="confirm-passphrase">Confirm Passphrase</Label>
                                <Input
                                    id="confirm-passphrase"
                                    type="password"
                                    value={confirmPassphrase}
                                    onChange={(e) => setConfirmPassphrase(e.target.value)}
                                    placeholder="Repeat passphrase"
                                />
                            </div>
                            <Alert>
                                <KeyRound className="h-4 w-4" />
                                <AlertTitle>Important</AlertTitle>
                                <AlertDescription>
                                    You will receive a one-time recovery key on the next step. Save it offline.
                                </AlertDescription>
                            </Alert>
                            <div className="flex gap-2">
                                <Button variant="outline" onClick={() => setStep("mode")} className="w-full">
                                    Back
                                </Button>
                                <Button onClick={handleInitializeEncryption} disabled={isBusy} className="w-full">
                                    {isBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                                    Generate Recovery Key
                                </Button>
                            </div>
                        </div>
                    )}

                    {step === "recovery" && (
                        <div className="space-y-4">
                            <Alert>
                                <TriangleAlert className="h-4 w-4" />
                                <AlertTitle>Recovery Key - Show Once Only</AlertTitle>
                                <AlertDescription>
                                    This key is displayed only now. Save it before you continue.
                                </AlertDescription>
                            </Alert>
                            <div className="rounded-md border bg-muted p-3 font-mono text-sm break-all">
                                {recoveryKey || "Hidden after verification"}
                            </div>
                            <div className="flex gap-2">
                                <Button variant="outline" onClick={downloadRecoveryKey} disabled={!recoveryKey}>
                                    Download
                                </Button>
                                <Button variant="outline" onClick={printRecoveryKey} disabled={!recoveryKey}>
                                    Print
                                </Button>
                                <Button variant="outline" onClick={() => navigator.clipboard.writeText(recoveryKey || "")} disabled={!recoveryKey}>
                                    Copy
                                </Button>
                            </div>

                            <Separator />

                            <div className="space-y-3">
                                <p className="text-sm font-medium">Verify recovery key to continue</p>
                                <div className="grid gap-3 md:grid-cols-2">
                                    <div className="space-y-1">
                                        <Label>
                                            Segment #{verifyIndexes[0] + 1}
                                        </Label>
                                        <Input value={verifyA} onChange={(e) => setVerifyA(e.target.value)} />
                                    </div>
                                    <div className="space-y-1">
                                        <Label>
                                            Segment #{verifyIndexes[1] + 1}
                                        </Label>
                                        <Input value={verifyB} onChange={(e) => setVerifyB(e.target.value)} />
                                    </div>
                                </div>
                                <Button variant="outline" onClick={handleVerifyRecovery}>
                                    Verify Recovery Key
                                </Button>
                                {recoveryVerified && (
                                    <p className="text-sm text-green-600 flex items-center gap-1">
                                        <Check className="h-4 w-4" />
                                        Recovery key verified.
                                    </p>
                                )}
                            </div>

                            <Button className="w-full" disabled={!recoveryVerified} onClick={handleConfirmRecoveryStep}>
                                Continue to BYOK Setup
                            </Button>
                        </div>
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
                                <Input
                                    id="api-hash"
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
