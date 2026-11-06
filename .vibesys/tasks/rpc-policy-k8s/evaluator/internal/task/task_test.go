package task

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/k8s"
)

// The task's manifest renders, and names the App's Deployments, caches and gateway.
func TestManifestRenders(t *testing.T) {
	app := App()
	if err := app.Validate(); err != nil {
		t.Fatal(err)
	}
	text, err := os.ReadFile(filepath.Join("..", "..", "..", app.Manifest))
	if err != nil {
		t.Fatal(err)
	}
	r, err := k8s.Render(string(text), "sn-test", map[string]string{"SOCIALNET_IMAGE": "registry/x:1"}, nil, app.Manifest)
	if err != nil {
		t.Fatal(err)
	}
	names := map[string]bool{}
	for _, o := range r.Objects() {
		if kind, _ := o["kind"].(string); kind == "Deployment" {
			meta, _ := o["metadata"].(map[string]any)
			name, _ := meta["name"].(string)
			names[name] = true
		}
	}
	for _, n := range app.Restartable {
		if !names[n] {
			t.Errorf("no Deployment %s", n)
		}
	}
	for n := range app.Caches {
		if !names[n] {
			t.Errorf("no Deployment %s", n)
		}
	}
	if names["home-timeline-redis"] && app.Caches["home-timeline-redis"] != "" {
		t.Error("home-timeline-redis has no store behind it and must not be flushed")
	}
}

func TestFaultsAreValid(t *testing.T) {
	if err := Task().Accuracy.Phases; err == 0 {
		t.Fatal("no phases")
	}
	if len(Faults()) == 0 {
		t.Fatal("no faults")
	}
}

func TestMedian(t *testing.T) {
	if m := median([]float64{3, 1, 2}); m != 2 {
		t.Fatal(m)
	}
	if m := median([]float64{4, 1}); m != 2.5 {
		t.Fatal(m)
	}
}
