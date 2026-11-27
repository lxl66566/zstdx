# Research strategy
## A floor label closes only the forms that were measured


The decode sequence loop was closed as a floor in 2026-11 after every single-change A/B re-rolled its register allocation, and the wholesale port of `ZSTD_decompressSequences_body` then moved the cells 14 to 51 percent in one commit. The R129 encode round repeated the mistake from the other side: the fastest scan's "floor" label steered every lane into scan-edge candidates (deferred events, inline packing, non-match pipeline) while the scan body's registered 2.25x per-event volume against the reference held the actual gap. Six lanes closed the edges and the body stayed untouched.

A floor label is a property of the measured forms, not of the code. Before scheduling lanes against a floor, check whether the reference body itself was ever ported as a unit, and whether the gap survives a reference-compiler differential (see the methodology page). When an after-action review finds a whole untouched component, record the follow-up as a todo item in the same round; conclusions that live only in conversation are lost by the next session.
