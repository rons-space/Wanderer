import { useMemo, useState } from "react";
import { toast } from "sonner";
import { api, errorMessage } from "@/lib/api";
import { splitRecoveryKey, verificationIndexes, verifySegments } from "@/lib/recovery-key";
import { Alert, AlertDescription, AlertTitle } from "./ui/alert";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Label } from "./ui/label";
import { Separator } from "./ui/separator";
import { Check, KeyRound, Loader2, TriangleAlert } from "lucide-react";

/**
 * Hands the recovery key to the user as a file. The object URL is revoked on a
 * timer rather than immediately after `click()`: the click only queues the
 * download, and revoking in the same tick cancels it in WebKit-based webviews,
 * which is every macOS build.
 */
function downloadRecoveryKey(recoveryKey: string) {
    const blob = new Blob([`Wander(er) Recovery Key\n\n${recoveryKey}\n`], {
        type: "text/plain;charset=utf-8",
    });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = "wanderer-recovery-key.txt";
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/**
 * Opens a print view. Returns false when the popup was blocked, which the
 * caller has to report: silently doing nothing here would leave a user
 * believing they had printed the only copy of their recovery key.
 */
function printRecoveryKey(recoveryKey: string): boolean {
    const printWindow = window.open("", "_blank", "width=700,height=500");
    if (!printWindow) {
        return false;
    }

    printWindow.document.write(
        `<pre style="font-family: ui-monospace, SFMono-Regular, Menlo, monospace; padding: 24px;">Wander(er) Recovery Key\n\n${recoveryKey}\n\nStore this securely. Anyone with this key can recover your vault.</pre>`,
    );
    printWindow.document.close();
    // The window used to be left open holding the key in plain sight. Closing
    // on afterprint rather than straight after print() lets the print dialog
    // finish reading the document first.
    printWindow.addEventListener("afterprint", () => printWindow.close());
    printWindow.focus();
    printWindow.print();
    return true;
}

async function copyRecoveryKey(recoveryKey: string) {
    try {
        await navigator.clipboard.writeText(recoveryKey);
        toast.success("Recovery key copied to clipboard.");
    } catch (e) {
        // Clipboard access is refused often enough (no permission, no focus)
        // that an unhandled rejection here would look like a successful copy.
        console.error("Failed to copy the recovery key:", e);
        toast.error("Could not copy the recovery key. Use Download or Print instead.");
    }
}

export interface EnableEncryptionProps {
    /** Runs once encryption is on, before the key is shown. */
    onEnabled?: () => void | Promise<void>;
    /** Runs after the user has verified the key and dismissed it. */
    onComplete: () => void;
    /** Rendered next to the submit button when present. */
    onBack?: () => void;
    continueLabel?: string;
}

/**
 * Turning on encryption, passphrase through to a verified recovery key.
 *
 * Settings used to have its own version of this that called
 * `initializeEncryption` and printed the key as bare text: no download, no
 * print, no copy, and no check that the user had kept it. Encryption is
 * one-way, so that path could leave a library permanently unrecoverable the
 * moment the passphrase was forgotten. Both entry points now run this flow.
 */
export function EnableEncryption({ onEnabled, onComplete, onBack, continueLabel = "Continue" }: EnableEncryptionProps) {
    const [passphrase, setPassphrase] = useState("");
    const [confirmPassphrase, setConfirmPassphrase] = useState("");
    const [isBusy, setIsBusy] = useState(false);

    const [recoveryKey, setRecoveryKey] = useState<string | null>(null);
    const [answerA, setAnswerA] = useState("");
    const [answerB, setAnswerB] = useState("");
    const [verified, setVerified] = useState(false);

    const segments = useMemo(() => (recoveryKey ? splitRecoveryKey(recoveryKey) : []), [recoveryKey]);
    const indexes = useMemo(() => verificationIndexes(segments.length), [segments.length]);

    const handleInitialize = async () => {
        if (passphrase.length < 8) {
            toast.error("Passphrase must be at least 8 characters.");
            return;
        }
        if (passphrase !== confirmPassphrase) {
            toast.error("Passphrase confirmation does not match.");
            return;
        }

        setIsBusy(true);
        try {
            const result = await api.initializeEncryption(passphrase);
            setRecoveryKey(result.recoveryKey.trim());
            // The passphrase has served its purpose; do not keep it in state
            // for the rest of the session.
            setPassphrase("");
            setConfirmPassphrase("");
            await onEnabled?.();
        } catch (e) {
            toast.error(`Failed to enable encryption: ${errorMessage(e)}`);
        } finally {
            setIsBusy(false);
        }
    };

    const handleVerify = () => {
        if (!verifySegments(segments, indexes, [answerA, answerB])) {
            toast.error("Recovery key verification failed.");
            return;
        }
        setVerified(true);
        toast.success("Recovery key verified.");
    };

    const handleFinish = () => {
        // Shown once: drop it from state so it cannot be read again by
        // reopening the panel.
        setRecoveryKey(null);
        setAnswerA("");
        setAnswerB("");
        setVerified(false);
        onComplete();
    };

    if (!recoveryKey) {
        return (
            <div className="space-y-4">
                <Alert>
                    <KeyRound className="h-4 w-4" aria-hidden="true" />
                    <AlertTitle>Enable Encryption (one-way)</AlertTitle>
                    <AlertDescription>
                        Once enabled you cannot switch back to unencrypted mode without a full reset. You will
                        receive a one-time recovery key on the next step.
                    </AlertDescription>
                </Alert>
                <div className="space-y-2">
                    <Label htmlFor="encryption-passphrase">Passphrase</Label>
                    <Input
                        id="encryption-passphrase"
                        type="password"
                        autoComplete="new-password"
                        value={passphrase}
                        onChange={(e) => setPassphrase(e.target.value)}
                        placeholder="At least 8 characters"
                    />
                </div>
                <div className="space-y-2">
                    <Label htmlFor="encryption-passphrase-confirm">Confirm Passphrase</Label>
                    <Input
                        id="encryption-passphrase-confirm"
                        type="password"
                        autoComplete="new-password"
                        value={confirmPassphrase}
                        onChange={(e) => setConfirmPassphrase(e.target.value)}
                        placeholder="Repeat passphrase"
                    />
                </div>
                <div className="flex gap-2">
                    {onBack && (
                        <Button variant="outline" className="w-full" onClick={onBack}>
                            Back
                        </Button>
                    )}
                    <Button className="w-full" onClick={handleInitialize} disabled={isBusy}>
                        {isBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
                        Generate Recovery Key
                    </Button>
                </div>
            </div>
        );
    }

    return (
        <div className="space-y-4">
            <Alert>
                <TriangleAlert className="h-4 w-4" aria-hidden="true" />
                <AlertTitle>Recovery Key - shown once only</AlertTitle>
                <AlertDescription>
                    This key is displayed only now. Save it before you continue: without it, a forgotten
                    passphrase means the library cannot be decrypted.
                </AlertDescription>
            </Alert>

            <div className="rounded-md border bg-muted p-3 font-mono text-sm break-all">{recoveryKey}</div>

            <div className="flex gap-2">
                <Button variant="outline" onClick={() => downloadRecoveryKey(recoveryKey)}>
                    Download
                </Button>
                <Button
                    variant="outline"
                    onClick={() => {
                        if (!printRecoveryKey(recoveryKey)) {
                            toast.error("The print window was blocked. Use Download or Copy instead.");
                        }
                    }}
                >
                    Print
                </Button>
                <Button variant="outline" onClick={() => void copyRecoveryKey(recoveryKey)}>
                    Copy
                </Button>
            </div>

            <Separator />

            <div className="space-y-3">
                <p className="text-sm font-medium">Verify the recovery key to continue</p>
                <div className="grid gap-3 md:grid-cols-2">
                    <div className="space-y-1">
                        <Label htmlFor="recovery-segment-a">Segment #{indexes[0] + 1}</Label>
                        <Input
                            id="recovery-segment-a"
                            value={answerA}
                            onChange={(e) => setAnswerA(e.target.value)}
                        />
                    </div>
                    <div className="space-y-1">
                        <Label htmlFor="recovery-segment-b">Segment #{indexes[1] + 1}</Label>
                        <Input
                            id="recovery-segment-b"
                            value={answerB}
                            onChange={(e) => setAnswerB(e.target.value)}
                        />
                    </div>
                </div>
                <Button variant="outline" onClick={handleVerify}>
                    Verify Recovery Key
                </Button>
                {verified && (
                    <p className="flex items-center gap-1 text-sm text-green-600">
                        <Check className="h-4 w-4" aria-hidden="true" />
                        Recovery key verified.
                    </p>
                )}
            </div>

            <Button className="w-full" disabled={!verified} onClick={handleFinish}>
                {continueLabel}
            </Button>
        </div>
    );
}
