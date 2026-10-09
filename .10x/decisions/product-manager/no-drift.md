# Product: no drift (1.2), 2026-10-09

**Who:** a team shipping one flow on iOS, Android and the web from one core, who wants to know, before a release, that
the three apps asked the core for the same things and got the same answers from their platforms; and the iOS engineer
who writes the app's one custom port.

**Scope (the spec's table is binding):** `undra drift REFERENCE OTHER...` with decoded arguments, built-in ignores and
`--exit-code`; the playground core's own drift test; ADR-066's closure form and compile test; the playground's `Locale`
port in the new form on iOS; the cookbook recipe "Your own port, on three platforms" with compiled snippets; the
Gate restructured; the landing page's 1.2 section, the roadmap's 1.2 group, the architecture page's "where drift is
caught", the README, the post.

**Success:** an engineer records a flow on two phones and the web with `undra dev --record`, runs one command, and
reads which platform diverged, on which call or port answer, with the values spelled out; and an iOS engineer registers
a custom port with one closure per method and no annotation, with a build that cannot regress.

**Out of scope:** adapter conformance (roadmap, Next, with the evidence); recordings produced in a user's CI from
devices (local developer tool first); a React Native platform name in recordings.
