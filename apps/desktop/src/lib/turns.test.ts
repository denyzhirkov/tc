import { test } from "node:test";
import assert from "node:assert/strict";
import { TURN_GAP, TURN_MIN, TurnTracker, type Turn } from "./turns.ts";

const closedBy = (tr: TurnTracker, now: number) => {
  const out: { turn: Turn; kept: boolean }[] = [];
  tr.sweep(now, (turn, kept) => out.push({ turn: { ...turn }, kept }));
  return out;
};

test("samples closer than the gap are one turn", () => {
  const tr = new TurnTracker();
  assert.ok(tr.hear("anna", 10));
  assert.equal(tr.hear("anna", 10.2), null);
  assert.equal(tr.hear("anna", 10.2 + TURN_GAP - 0.1), null);
  assert.deepEqual(closedBy(tr, 10.3 + TURN_GAP), []);
  assert.equal(tr.current("anna")?.start, 10);
});

test("silence past the gap closes the turn at its last sample", () => {
  const tr = new TurnTracker();
  tr.hear("anna", 10);
  tr.hear("anna", 12);
  const closed = closedBy(tr, 12 + TURN_GAP);
  assert.deepEqual(closed, [{ turn: { who: "anna", start: 10, end: 12 }, kept: true }]);
  assert.equal(tr.current("anna"), undefined);
  // The next sample is a new turn, not a continuation.
  assert.equal(tr.hear("anna", 20)?.start, 20);
});

test("a blip shorter than the minimum is reported as not kept", () => {
  const tr = new TurnTracker();
  tr.hear("boris", 5);
  tr.hear("boris", 5 + TURN_MIN - 0.2);
  assert.equal(closedBy(tr, 5 + TURN_MIN + TURN_GAP)[0].kept, false);
});

test("people are tracked apart and a rename follows the open turn", () => {
  const tr = new TurnTracker();
  tr.hear("anna", 1);
  tr.hear("boris", 1.5);
  tr.rename("anna", "ann");
  assert.equal(tr.current("anna"), undefined);
  assert.equal(tr.hear("ann", 3), null);
  const closed = closedBy(tr, 1.5 + TURN_GAP);
  assert.deepEqual(
    closed.map((c) => c.turn.who),
    ["boris"],
  );
  assert.equal(tr.current("ann")?.who, "ann");
});
