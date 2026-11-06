package workload

import (
	"testing"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/workload"

	"rpc-policy-k8s/evaluator/internal/social"
)

// stream builds a phase's operations the way the generator does: a weighted draw of the mix,
// then the operation's Build on its own stream.
func stream(t *testing.T, w workload.Workload[*social.Dataset, *Model], phase, n int) ([]workload.Request, []string) {
	t.Helper()
	m, err := w.Model(social.Default(), workload.NewRand(7, "model"))
	if err != nil {
		t.Fatal(err)
	}
	if err := w.BeginPhase(m, phase); err != nil {
		t.Fatal(err)
	}
	weights := make([]float64, len(w.Operations))
	for i, op := range w.Operations {
		weights[i] = op.Weight
	}
	mix := workload.NewRand(7, "mix", phase)
	var reqs []workload.Request
	var names []string
	for i := 0; i < n; i++ {
		op := w.Operations[mix.Weighted(weights)]
		reqs = append(reqs, op.Build(m, workload.NewRand(7, "op", phase, i)))
		names = append(names, op.Name)
	}
	return reqs, names
}

// Every operation of a round is a different hot user's, a user's operations are a whole round
// apart, mentions are of other hot users, markers are unique, and a follow change is of a user
// that posted a round or more earlier in the phase.
func TestAccuracyStream(t *testing.T) {
	data := social.Default()
	n := 4000
	for phase := 0; phase < AccuracyPhases; phase++ {
		reqs, names := stream(t, Accuracy(), phase, n)
		markers := map[string]bool{}
		lastPost := map[int64]int{}
		kinds := map[string]int{}
		for i, r := range reqs {
			a := r.Info.(social.Action)
			if want := int64(i%data.Hot) + 1; a.User != want {
				t.Fatalf("phase %d op %d (%s) is user %d's, want %d", phase, i, names[i], a.User, want)
			}
			kinds[names[i]]++
			switch {
			case a.Kind == social.KindCompose && a.Invalid == "":
				if markers[a.Marker] {
					t.Fatalf("marker %s twice", a.Marker)
				}
				markers[a.Marker] = true
				lastPost[a.User] = i
				if a.Mention != 0 && (a.Mention == a.User || !data.IsHot(a.Mention)) {
					t.Fatalf("op %d mentions %d", i, a.Mention)
				}
			case (a.Kind == social.KindFollow || a.Kind == social.KindUnfollow) && a.Invalid == "":
				j, ok := lastPost[a.Target]
				if !ok || i-j < data.Hot {
					t.Fatalf("phase %d op %d: %s %d -> %d, whose last post was at %d", phase, i, a.Kind, a.User, a.Target, j)
				}
				if !data.IsHot(a.Target) || data.Follows(a.User, a.Target) {
					t.Fatalf("op %d: %s %d -> %d is a seeded edge or not hot", i, a.Kind, a.User, a.Target)
				}
			}
		}
		for _, k := range []string{"compose", "home_read", "user_read", "follow", "unfollow", "invalid"} {
			if kinds[k] == 0 {
				t.Errorf("phase %d: no %s", phase, k)
			}
		}
		t.Logf("phase %d: %v", phase, kinds)
	}
}

func TestBenchStream(t *testing.T) {
	reqs, names := stream(t, Bench(), 0, 3000)
	count := map[string]int{}
	for i, r := range reqs {
		count[names[i]]++
		if len(r.Steps) == 0 || r.Info != nil {
			t.Fatalf("%s: %d steps, info %v", names[i], len(r.Steps), r.Info)
		}
	}
	if count["user_timeline_read"] < 1300 || count["home_timeline_read"] < 1000 || count["compose_post"] < 200 {
		t.Errorf("mix %v", count)
	}
}

func TestDatasetIsStable(t *testing.T) {
	a, b := social.Default(), social.Default()
	if a.Edges() != b.Edges() || a.Edges() < 8*social.DefaultUsers {
		t.Fatalf("edges %d, %d", a.Edges(), b.Edges())
	}
	hotEdges := 0
	for u := int64(1); u <= int64(a.Hot); u++ {
		for _, v := range a.Followees(u) {
			if a.IsHot(int64(v)) {
				hotEdges++
			}
		}
	}
	t.Logf("%d users, %d edges, %d of them among the %d hot users", a.Users, a.Edges(), hotEdges, a.Hot)
	if hotEdges == 0 {
		t.Fatal("the hot users do not follow each other")
	}
}
