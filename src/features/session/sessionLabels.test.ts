import { endSessionLabel } from "./sessionLabels";

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(`assertion failed: ${message}`);
}

function run(name: string, fn: () => void): void {
  fn();
  console.log(`ok: ${name}`);
}

run("end session label keeps timer inside the red end action", () => {
  assert(endSessionLabel("08:14") === "Encerrar • 08:14", "elapsed time is part of the end button label");
});
