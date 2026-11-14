// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// The update found by the startup check or by "Check for updates", shared
// by the prompt in App.svelte and the settings view. Nothing installs
// until the user presses Install in the prompt.

import type { UpdateAvailable } from "./native";

export const pendingUpdate = $state<{ update: UpdateAvailable | null }>({ update: null });
