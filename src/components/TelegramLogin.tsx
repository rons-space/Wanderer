import { useCallback, useState } from "react";
import { api, errorMessage } from "@/lib/api";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Label } from "./ui/label";
import { Loader2 } from "lucide-react";

export type TelegramLoginStep = "phone" | "code";

export interface TelegramLoginState {
    phone: string;
    setPhone: (value: string) => void;
    code: string;
    setCode: (value: string) => void;
    step: TelegramLoginStep;
    /** Returns to the phone entry step, discarding the code that was typed. */
    backToPhone: () => void;
    isBusy: boolean;
    /** Last failure, for surfaces that render errors inline instead of as a toast. */
    error: string | null;
    requestCode: () => Promise<void>;
    signIn: () => Promise<void>;
}

export interface UseTelegramLoginOptions {
    /** Called with the signed-in username once Telegram accepts the code. */
    onAuthenticated?: (user: string) => void;
    /**
     * Called instead of storing the message when a step fails. Onboarding shows
     * toasts, Settings renders the stored error inline, so both are supported.
     */
    onError?: (message: string) => void;
}

/**
 * The phone-then-code state machine behind every Telegram sign-in surface.
 *
 * Settings and Onboarding used to carry a copy of this each, which is how they
 * drifted: one reset the step on failure and the other did not, and only one of
 * them raised `auth-changed`. Keeping the transitions here means a fix lands in
 * both places at once.
 */
export function useTelegramLogin(options: UseTelegramLoginOptions = {}): TelegramLoginState {
    const { onAuthenticated, onError } = options;
    const [phone, setPhone] = useState("");
    const [code, setCode] = useState("");
    const [step, setStep] = useState<TelegramLoginStep>("phone");
    const [isBusy, setIsBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);

    const fail = useCallback(
        (message: string) => {
            setError(message);
            onError?.(message);
        },
        [onError],
    );

    const requestCode = useCallback(async () => {
        const trimmed = phone.trim();
        if (!trimmed) {
            fail("Phone number is required.");
            return;
        }
        setIsBusy(true);
        setError(null);
        try {
            await api.loginRequestCode(trimmed);
            setStep("code");
        } catch (e) {
            fail(`Failed to send code: ${errorMessage(e)}`);
        } finally {
            setIsBusy(false);
        }
    }, [fail, phone]);

    const signIn = useCallback(async () => {
        const trimmed = code.trim();
        if (!trimmed) {
            fail("Verification code is required.");
            return;
        }
        setIsBusy(true);
        setError(null);
        try {
            const user = await api.loginSignIn(trimmed);
            // Other views listen for this rather than polling `getMe`.
            window.dispatchEvent(new Event("auth-changed"));
            onAuthenticated?.(user);
        } catch (e) {
            const message = errorMessage(e);
            fail(`Failed to sign in: ${message}`);
            // A mistyped code can simply be retyped: Telegram keeps the pending
            // request alive. Only once the backend has forgotten the request is
            // a new code needed, and then the phone number has to be re-entered.
            if (message.includes("No pending login request")) {
                setStep("phone");
                setCode("");
            }
        } finally {
            setIsBusy(false);
        }
    }, [code, fail, onAuthenticated]);

    const backToPhone = useCallback(() => {
        setStep("phone");
        setCode("");
        setError(null);
    }, []);

    return {
        phone,
        setPhone,
        code,
        setCode,
        step,
        backToPhone,
        isBusy,
        error,
        requestCode,
        signIn,
    };
}

export interface TelegramLoginFormProps {
    login: TelegramLoginState;
    /** Rendered above the fields; Settings uses it for its inline error box. */
    banner?: React.ReactNode;
    /** Distinguishes the input ids when two forms share a page. */
    idPrefix?: string;
}

/**
 * The shared phone/code form. Callers own the surrounding card, heading and
 * whatever they show once `onAuthenticated` has fired.
 */
export function TelegramLoginForm({ login, banner, idPrefix = "telegram" }: TelegramLoginFormProps) {
    const { phone, setPhone, code, setCode, step, backToPhone, isBusy, requestCode, signIn } = login;
    const phoneId = `${idPrefix}-phone`;
    const codeId = `${idPrefix}-code`;

    return (
        <form
            className="space-y-4"
            onSubmit={(e) => {
                e.preventDefault();
                void (step === "phone" ? requestCode() : signIn());
            }}
        >
            {banner}
            {step === "phone" ? (
                <div className="space-y-2">
                    <Label htmlFor={phoneId}>Phone Number</Label>
                    <Input
                        id={phoneId}
                        value={phone}
                        onChange={(e) => setPhone(e.target.value)}
                        placeholder="+1234567890"
                        autoComplete="tel"
                        required
                    />
                    <p className="text-muted-foreground text-xs">Include country code</p>
                </div>
            ) : (
                <div className="space-y-2">
                    <Label htmlFor={codeId}>Verification Code</Label>
                    <Input
                        id={codeId}
                        value={code}
                        onChange={(e) => setCode(e.target.value)}
                        placeholder="12345"
                        inputMode="numeric"
                        autoComplete="one-time-code"
                        required
                    />
                </div>
            )}
            <Button type="submit" className="w-full" disabled={isBusy}>
                {isBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                {step === "phone" ? "Send Code" : "Sign In"}
            </Button>
            {step === "code" && (
                <Button variant="outline" type="button" className="w-full" onClick={backToPhone}>
                    Back to Phone Number
                </Button>
            )}
        </form>
    );
}
