// Package social is the application as the task sees it: the users and follow graph the seed
// registers over the HTTP API, the HTTP routes the workloads call, and how the application
// answers them (the posts it serves, the success messages, how it rewrites a post's text).
package social

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strconv"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/workload"
)

// The dataset's defaults: DefaultUsers users, each following FollowsMin to FollowsMax others
// drawn by popularity (Zipf with exponent PopularityExponent over the user ids: user 1 is the
// most followed), and the Hot most popular users whom the accuracy check's traffic is on.
const (
	DefaultSeed        = 20261106
	DefaultUsers       = 1000
	FollowsMin         = 8
	FollowsMax         = 40
	PopularityExponent = 0.9
	Hot                = 40
)

// Dataset is the data the seed writes into a fresh application through its HTTP API: Users
// users, registered with ids 1..Users (username "user<id>", password "pw<id>"), and the follow
// graph Seed generates. The application starts empty and seeds nothing of its own except the
// user-mention service's table of 1,000,000 fake users "user1".."user1000000" (ids 1..1000000),
// which is why the users are named so: a mention "@user<id>" resolves to the user <id>.
//
// Only the configuration is encoded (what the evaluator writes to dataset.json and compares
// between processes); the graph is regenerated from it.
type Dataset struct {
	Seed  uint64 `json:"seed"`
	Users int    `json:"users"`
	Hot   int    `json:"hot"`

	followees [][]int32 // by user index (id - 1): the ids it follows, ascending
	followers [][]int32 // by user index: the ids that follow it, ascending
	edges     int
	active    []int32 // user ids, most active first (a permutation from the seed)
}

// Default is the task's dataset.
func Default() *Dataset { return New(DefaultSeed, DefaultUsers, Hot) }

// New generates the dataset of seed with users users and hot hot users.
func New(seed uint64, users, hot int) *Dataset {
	d := &Dataset{Seed: seed, Users: users, Hot: hot}
	d.build()
	return d
}

func (d *Dataset) build() {
	n := d.Users
	d.followees = make([][]int32, n)
	d.followers = make([][]int32, n)
	d.edges = 0
	rand := workload.NewRand(d.Seed, "graph")
	for u := 0; u < n; u++ {
		r := rand.Child("user", u)
		want := FollowsMin + r.Below(FollowsMax-FollowsMin+1)
		picked := map[int32]bool{}
		for tries := 0; len(picked) < want && tries < 20*want; tries++ {
			v := int32(r.Zipf(n, PopularityExponent)) // index
			if int(v) != u {
				picked[v+1] = true
			}
		}
		for v := range picked {
			d.followees[u] = append(d.followees[u], v)
			d.followers[v-1] = append(d.followers[v-1], int32(u+1))
		}
		d.edges += len(picked)
	}
	for u := 0; u < n; u++ {
		sort.Slice(d.followees[u], func(i, j int) bool { return d.followees[u][i] < d.followees[u][j] })
		sort.Slice(d.followers[u], func(i, j int) bool { return d.followers[u][i] < d.followers[u][j] })
	}
	d.active = make([]int32, n)
	for i := range d.active {
		d.active[i] = int32(i + 1)
	}
	perm := workload.NewRand(d.Seed, "activity")
	for i := n - 1; i > 0; i-- {
		k := perm.Below(i + 1)
		d.active[i], d.active[k] = d.active[k], d.active[i]
	}
}

// ReadDataset reads a dataset file (the JSON of a Dataset) and regenerates its graph.
func ReadDataset(path string) (*Dataset, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var d Dataset
	if err := json.Unmarshal(raw, &d); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if d.Users < 2 || d.Hot < 2 || d.Hot > d.Users || d.Hot > 64 {
		return nil, fmt.Errorf("%s: a dataset of %d users with %d hot ones (at most 64)", path, d.Users, d.Hot)
	}
	d.build()
	return &d, nil
}

// Edges is the number of follow edges.
func (d *Dataset) Edges() int { return d.edges }

// Followees are the ids user id follows, ascending.
func (d *Dataset) Followees(id int64) []int32 { return d.followees[id-1] }

// Followers are the ids that follow user id, ascending.
func (d *Dataset) Followers(id int64) []int32 { return d.followers[id-1] }

// Follows says whether u follows v in the seeded graph.
func (d *Dataset) Follows(u, v int64) bool {
	if u < 1 || v < 1 || int(u) > d.Users || int(v) > d.Users {
		return false
	}
	f := d.followees[u-1]
	i := sort.Search(len(f), func(i int) bool { return int64(f[i]) >= v })
	return i < len(f) && int64(f[i]) == v
}

// IsHot says whether id is one of the hot users (ids 1..Hot).
func (d *Dataset) IsHot(id int64) bool { return id >= 1 && id <= int64(d.Hot) }

// HotFollowees is the set of hot users the hot user u follows in the seeded graph, as a bit set
// (bit v-1 for user v).
func (d *Dataset) HotFollowees(u int64) uint64 {
	var set uint64
	for _, v := range d.Followees(u) {
		if d.IsHot(int64(v)) {
			set |= 1 << (v - 1)
		}
	}
	return set
}

// Active is the user id of activity rank r (0 is the most active).
func (d *Dataset) Active(r int) int64 { return int64(d.active[r]) }

// Username and Password are user id's.
func Username(id int64) string { return "user" + strconv.FormatInt(id, 10) }
func Password(id int64) string { return "pw" + strconv.FormatInt(id, 10) }
