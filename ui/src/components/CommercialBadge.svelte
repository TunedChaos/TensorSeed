<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import type { CommercialAssessment } from "../types";

  let { assessment }: { assessment: CommercialAssessment } = $props();

  const styles: Record<CommercialAssessment["value"], string> = {
    allowed: "badge-success",
    restricted: "badge-warning",
    forbidden: "badge-error",
    unknown: "badge-ghost",
  };
  const labels: Record<CommercialAssessment["value"], string> = {
    allowed: "Commercial: allowed",
    restricted: "Commercial: restricted",
    forbidden: "Commercial: forbidden",
    unknown: "Commercial: unknown",
  };
</script>

<span
  class="badge badge-sm ms-1 {styles[assessment.value]}"
  title={`${assessment.note}. Not legal advice.`}
  data-commercial={assessment.value}
>{labels[assessment.value]}</span>
