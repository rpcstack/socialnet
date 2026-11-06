// Package task wires the task on vibesys-evaluator's microservices runner: the application on
// Kubernetes, the benchmark, the accuracy check and the edit-scope guard (cmd/rpc-policy-k8s).
package task

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/check"
	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/evaluator"
	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/k8s"
	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/microservices"
	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/workload"

	"rpc-policy-k8s/evaluator/internal/oracle"
	"rpc-policy-k8s/evaluator/internal/scope"
	"rpc-policy-k8s/evaluator/internal/social"
	wl "rpc-policy-k8s/evaluator/internal/workload"
)

// The benchmark: capacity in a closed loop, then the p99 latency at a fixed offered rate in an
// open loop. The numbers come from the base's runs on vibesys-k8s (README.md, "Choosing the loads").
const (
	ClosedClients = 32
	ClosedTrials  = 2
	ClosedSecs    = 60 * time.Second
	ClosedWarmup  = 15 * time.Second

	OpenRate   = 800.0
	OpenTrials = 3
	OpenSecs   = 60 * time.Second
	OpenWarmup = 10 * time.Second
)

// The accuracy check: AccuracyPhases phases of AccuracySecs at AccuracyRate, with faults between
// them.
const (
	AccuracyRate = 300.0
	AccuracySecs = 45 * time.Second
)

// Gateway is the frontend's HTTP API, the base URL of every request.
const Gateway = "http://frontend-service:5000"

// TargetEnv names the environment variable that runs the benchmark or the accuracy check
// against an application already running at its URL (docker compose; README.md, "Run it
// locally") instead of building and deploying the candidate on Kubernetes. The seed is written
// by the task as for a deployment, and the accuracy check then injects no faults.
const TargetEnv = "SOCIALNET_TARGET"

func system() microservices.System {
	if target := os.Getenv(TargetEnv); target != "" {
		return microservices.Local(target)
	}
	return microservices.K8s(App(), Ready()...)
}

// Metrics are the benchmark's objectives.
var Metrics = map[string]evaluator.Metric{
	"operations_per_second": {Unit: "ops/s", Direction: evaluator.Max},
	"latency_ms.p99":        {Unit: "ms", Direction: evaluator.Min},
}

// Validity: no error in any window (a policy may not shed load by failing requests), every
// operation of the mix completed, the open loop offering at least 95% of its rate.
var Validity = evaluator.Validity{MaxErrorRate: 0, MinSustainedRatio: 0.95, MinOpsPerOperation: 1}

// The Deployments the accuracy check's faults act on. The services keep no state of their own:
// every one can be restarted. The caches are those with a store behind them (MongoDB, or the
// seed of the user-mention table); home-timeline-redis is the home timelines' only copy, so it
// is neither flushed nor restarted. MongoDB's data lives in an emptyDir, so it is not restartable.
var (
	Services = []string{
		"frontend-service", "compose-post-service", "register-user-service", "user-service",
		"social-graph-service", "user-timeline-service", "home-timeline-service", "post-storage-service",
		"user-mention-service", "url-shorten-service", "text-service", "media-service", "unique-id-service",
	}
	Caches = map[string]k8s.CacheKind{
		"user-timeline-redis": k8s.Redis, "social-graph-redis": k8s.Redis, "user-redis": k8s.Redis,
		"post-storage-memcached": k8s.Memcached, "user-mention-memcached": k8s.Memcached,
	}
)

// App is the social network on Kubernetes: the repository's manifests (k8s/base, rendered into
// k8s/socialnet.yaml by k8s/render.sh) with one image built from the candidate's root Dockerfile.
func App() k8s.App {
	return k8s.App{
		Name:        "rpc-policy-k8s",
		Manifest:    "k8s/socialnet.yaml",
		Images:      map[string]string{"SOCIALNET_IMAGE": "."},
		Gateway:     Gateway,
		Restartable: Services,
		Caches:      Caches,
		// The user-mention service fills a table of a million users before it serves.
		WaitSecs: 600,
	}
}

// Ready are the requests that must succeed before a sample's phases start, once the seed has
// run: a read of each timeline of the most followed user.
func Ready() []workload.Step {
	return []workload.Step{social.ReadPage(false, 1, 0, social.PageSize), social.ReadPage(true, 1, 0, social.PageSize)}
}

// Dataset is the users and follow graph the seed registers.
func Dataset() (*social.Dataset, error) { return social.Default(), nil }

// Seed registers the dataset through the frontend.
func Seed(ctx context.Context, d *social.Dataset) error {
	base := Gateway
	if target := os.Getenv(TargetEnv); target != "" {
		base = target
	}
	return d.Populate(ctx, base)
}

// Task is the task: the benchmark (the DeathStarBench mix) and the accuracy check (its own
// workload, faults and oracle).
func Task() microservices.Task[*social.Dataset, *wl.Model] {
	bench := wl.Bench()
	acc := wl.Accuracy()
	faults := Faults()
	if os.Getenv(TargetEnv) != "" {
		faults = nil
	}
	return microservices.Task[*social.Dataset, *wl.Model]{
		Metrics:   Metrics,
		Validity:  &Validity,
		Dataset:   Dataset,
		System:    system(),
		Seed:      Seed,
		Workload:  &bench,
		Benchmark: runBenchmark,
		Accuracy: &microservices.Accuracy[*social.Dataset, *wl.Model]{
			Phases:   wl.AccuracyPhases,
			Load:     microservices.Open{Rate: AccuracyRate, Secs: AccuracySecs, Arrivals: microservices.Uniform},
			Faults:   faults,
			Oracle:   oracle.Specs,
			Workload: &acc,
		},
	}
}

func runBenchmark(ctx context.Context, run *microservices.Run) error {
	var closed []float64
	for range ClosedTrials {
		r, err := run.ClosedLoop(ctx, microservices.Closed{Clients: ClosedClients, Secs: ClosedSecs, Warmup: ClosedWarmup})
		if err != nil {
			return err
		}
		closed = append(closed, r.OpsPerSec)
	}
	var p99 []float64
	for range OpenTrials {
		r, err := run.OpenLoop(ctx, microservices.Open{Rate: OpenRate, Secs: OpenSecs, Warmup: OpenWarmup, Arrivals: microservices.Uniform})
		if err != nil {
			return err
		}
		p99 = append(p99, r.P99Ms)
	}
	return run.Report(map[string]float64{"operations_per_second": median(closed), "latency_ms.p99": median(p99)})
}

func median(v []float64) float64 {
	s := append([]float64(nil), v...)
	sort.Float64s(s)
	if n := len(s); n%2 == 1 {
		return s[n/2]
	} else {
		return (s[n/2-1] + s[n/2]) / 2
	}
}

// Faults are the accuracy run's faults between its phases: the application's services restarted
// one pod at a time, the five caches emptied in place, then every service and the Redis pods
// restarted at once.
func Faults() []microservices.Fault {
	var f []microservices.Fault
	for _, s := range []string{"frontend-service", "compose-post-service", "user-timeline-service", "home-timeline-service", "social-graph-service", "post-storage-service"} {
		f = append(f, microservices.Restart(0, s))
	}
	caches := make([]string, 0, len(Caches))
	for c := range Caches {
		caches = append(caches, c)
	}
	sort.Strings(caches)
	for _, c := range caches {
		f = append(f, microservices.Flush(1, c))
	}
	for _, s := range Services {
		f = append(f, microservices.RestartAll(2, s))
	}
	for _, c := range []string{"user-timeline-redis", "social-graph-redis"} {
		f = append(f, microservices.Restart(2, c))
	}
	return f
}

// ------------------------------------------------------------------ the scope guard

// Guard checks the candidate's edit scope before a `benchmark` or `accuracy` command builds
// anything (other commands, and an accuracy check of a captured history, pass through). Outside
// the scope, the benchmark gets an error record and the accuracy check FAIL (exit 1).
func Guard() {
	if len(os.Args) < 2 || (os.Args[1] != "benchmark" && os.Args[1] != "accuracy") {
		return
	}
	root := "."
	for i, a := range os.Args[2:] {
		switch {
		case a == "--history" || strings.HasPrefix(a, "--history="):
			return
		case a == "--project-root" && i+3 < len(os.Args):
			root = os.Args[i+3]
		case strings.HasPrefix(a, "--project-root="):
			root = strings.TrimPrefix(a, "--project-root=")
		}
	}
	problems := checkScope(root)
	if len(problems) == 0 {
		return
	}
	msg := "the candidate edits outside policy/ (the task's edit scope; OBJECTIVE.md): " + strings.Join(problems, "; ")
	if os.Args[1] == "accuracy" {
		fmt.Fprintln(os.Stderr, "FAIL:", msg)
		os.Exit(check.Fail.ExitCode())
	}
	rest := []string{os.Args[0], "benchmark"}
	for i := 2; i < len(os.Args); i++ {
		if os.Args[i] == "--project-root" {
			i++
			continue
		}
		if !strings.HasPrefix(os.Args[i], "--project-root=") {
			rest = append(rest, os.Args[i])
		}
	}
	os.Args = rest
	evaluator.Main(evaluator.Task{
		Metrics:   Metrics,
		Benchmark: func(context.Context, *evaluator.Run) error { return &evaluator.SampleError{Msg: msg} },
	})
}

func checkScope(root string) []string {
	exe, err := os.Executable()
	if err != nil {
		return []string{fmt.Sprintf("the task cannot find its scope manifest: %v", err)}
	}
	pinned, err := scope.Load(filepath.Join(filepath.Dir(exe), scope.File))
	if err != nil {
		return []string{fmt.Sprintf("the task's scope manifest: %v (a task bug)", err)}
	}
	return scope.Check(root, pinned)
}
