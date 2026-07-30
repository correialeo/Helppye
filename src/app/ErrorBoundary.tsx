import { Component, type ReactNode } from "react";
import { PrimaryButton } from "../components/ui/PrimaryButton";
import { BrandMark } from "../components/ui/BrandMark";

interface ErrorBoundaryState {
  error: Error | null;
}

/** Catches render-time crashes anywhere in the screen tree so a bug in one screen never
 * takes down the whole app with a blank window — the one thing worse than a bug is a
 * bug with no way back to a working state. */
export class ErrorBoundary extends Component<{ children: ReactNode }, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  render() {
    if (this.state.error) {
      return (
        <div className="flex h-full min-h-screen w-full flex-col items-center justify-center gap-4 bg-app px-8 text-center">
          <BrandMark size={36} />
          <p className="text-sm text-neutral-300">Algo deu errado.</p>
          <PrimaryButton onClick={() => this.setState({ error: null })}>Tentar novamente</PrimaryButton>
        </div>
      );
    }
    return this.props.children;
  }
}
