package workload

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/workload"

	"rpc-policy-k8s/evaluator/internal/social"
)

// The accuracy workload. It runs at a fixed open-loop rate (task.AccuracyRate) in AccuracyPhases
// phases, on the dataset's hot users (ids 1..social.Hot, the most followed), and is built so
// that the application as it stands (the no-op policy) answers every request the way the
// oracle's model says, while a policy that serves stale, lost, duplicated or fabricated answers
// does not:
//
//   - Operations go round the hot users: operation i of a phase is user (i mod Hot)'s, whose
//     post, follow change or read (of its own user timeline or home timeline) it is. A user's
//     operations are therefore a whole round apart (Hot / rate, 200 ms at 200 ops/s), and never
//     overlap: the user-timeline service reads Redis and then MongoDB without a lock, so a read
//     racing a post of the same author after a cache flush can return a page that skips an entry
//     (the application's race, not a policy's).
//   - The social-graph service adds a follow to the followee's cached follower set even when the
//     cache does not hold it (after a flush), which leaves a partial set that later posts are
//     delivered by. A follow change of u -> v is therefore sent only after v posted at least one
//     round earlier in the phase (a post reads v's followers and fills the cache), and otherwise
//     becomes an invalid follow (no followee: 400, nothing changes). Follow changes are between
//     hot users and never remove a seeded edge, so a hot user's follower set never empties (an
//     empty set is refilled from MongoDB by the next post, racing an unfollow).
//   - Every post has a unique marker ("#m..."), mentions another hot user 20% of the time
//     ("@user<id>": the user-mention service resolves it to the user <id>, who gets the post on
//     its home timeline), ends with a URL 10% and has media 5%.
//   - Reads are of a page of PageSize entries: the first 70%, the second 20%, the third 7%, pages
//     4-7 3%.
//   - A few requests are invalid on purpose: a post without text, a follow without a followee, a
//     read of a non-numeric user, of an empty window or of a negative start.
const (
	AccuracyPhases = 4
	// HotOnly: the workload's users are the dataset's hot users.
	composeAcc  = 24
	homeAcc     = 34
	userAcc     = 26
	followAcc   = 6
	unfollowAcc = 6
	invalidAcc  = 4
	maxPhases   = 64
)

// Model is a phase's model, the benchmark's or the accuracy check's (the two workloads of a task
// share its type).
type Model struct {
	data   *social.Dataset
	prefix string // the sample's marker namespace
	seq    int64

	// The benchmark's: follower, followee -> following after this phase's changes.
	changed map[[2]int64]bool

	// The accuracy check's: operations built in this phase, and the operation index of each hot
	// user's last post in it (-1: none yet).
	ops      int
	lastPost []int
}

func newModel(data *social.Dataset, rand *workload.Rand) (*Model, error) {
	if data.Hot < 2 {
		return nil, fmt.Errorf("the dataset has %d hot users", data.Hot)
	}
	return &Model{
		data: data, prefix: fmt.Sprintf("m%010x", rand.Child("markers").Uint64()>>24), changed: map[[2]int64]bool{},
		lastPost: make([]int, data.Hot+1),
	}, nil
}

func beginAccuracyPhase(m *Model, phase int) error {
	if phase >= maxPhases {
		return fmt.Errorf("phase %d: the accuracy workload has marker blocks for %d phases", phase, maxPhases)
	}
	m.seq = int64(phase) << 32
	m.ops = 0
	for i := range m.lastPost {
		m.lastPost[i] = -1
	}
	return nil
}

// slot is the hot user of the next operation, and advances the round.
func (m *Model) slot() int64 {
	u := int64(m.ops%m.data.Hot) + 1
	m.ops++
	return u
}

func accPage(rand *workload.Rand) int {
	switch x := rand.Below(100); {
	case x < 70:
		return 0
	case x < 90:
		return social.PageSize
	case x < 97:
		return 2 * social.PageSize
	default:
		return (3 + rand.Below(4)) * social.PageSize
	}
}

func (m *Model) compose(rand *workload.Rand) social.Action {
	u := m.slot()
	m.lastPost[u] = m.ops - 1
	a := social.Action{Kind: social.KindCompose, User: u, Marker: m.prefix + strconv.FormatInt(m.seq, 36)}
	m.seq++
	parts := Words(rand)
	if rand.Random() < 0.2 {
		v := int64(1 + rand.Below(m.data.Hot-1))
		if v >= u {
			v++
		}
		a.Mention = v
		at := rand.Below(len(parts) + 1)
		parts = append(parts[:at], append([]string{"@" + social.Username(v)}, parts[at:]...)...)
	}
	parts = append(parts, "#"+a.Marker)
	if rand.Random() < 0.10 {
		a.URL = URL(rand)
		parts = append(parts, a.URL)
	}
	a.Text = strings.Join(parts, " ")
	if rand.Random() < 0.05 {
		a.Media = MediaOf(rand)
	}
	return a
}

// edge is a follow change by the slot's user u of a hot user v it does not follow in the seeded
// graph, whose last post in this phase was a round or more before; without one, a follow
// without a followee.
func (m *Model) edge(kind string) func(*workload.Rand) social.Action {
	return func(rand *workload.Rand) social.Action {
		u := m.slot()
		now := m.ops - 1
		var ok []int64
		for v := int64(1); v <= int64(m.data.Hot); v++ {
			if v != u && !m.data.Follows(u, v) && m.lastPost[v] >= 0 && now-m.lastPost[v] >= m.data.Hot {
				ok = append(ok, v)
			}
		}
		if len(ok) == 0 {
			return social.Action{Kind: kind, User: u, Invalid: social.InvalidNoFollowee}
		}
		return social.Action{Kind: kind, User: u, Target: ok[rand.Below(len(ok))]}
	}
}

func (m *Model) read(home bool) func(*workload.Rand) social.Action {
	return func(rand *workload.Rand) social.Action {
		start := accPage(rand)
		return social.Action{Kind: social.KindRead, User: m.slot(), Home: home, Start: start, Stop: start + social.PageSize}
	}
}

func (m *Model) invalid(rand *workload.Rand) social.Action {
	u := m.slot()
	switch rand.Below(5) {
	case 0:
		return social.Action{Kind: social.KindCompose, User: u, Invalid: social.InvalidNoText}
	case 1:
		return social.Action{Kind: social.KindFollow, User: u, Invalid: social.InvalidNoFollowee}
	case 2:
		return social.Action{Kind: social.KindRead, User: u, Home: rand.Below(2) == 0, Invalid: social.InvalidUser}
	case 3:
		return social.Action{Kind: social.KindRead, User: u, Home: rand.Below(2) == 0, Invalid: social.InvalidEmptyWindow}
	}
	return social.Action{Kind: social.KindRead, User: u, Home: rand.Below(2) == 0, Invalid: social.InvalidNegative}
}

// action makes an operation of an action builder: the action is the Info, its request the step.
func action(build func(*Model) func(*workload.Rand) social.Action) func(*Model, *workload.Rand) workload.Request {
	return func(m *Model, rand *workload.Rand) workload.Request {
		a := build(m)(rand)
		return workload.Request{Steps: []workload.Step{a.Step()}, Info: a}
	}
}

// Accuracy is the accuracy check's workload.
func Accuracy() workload.Workload[*social.Dataset, *Model] {
	return workload.Workload[*social.Dataset, *Model]{
		Dataset:    social.ReadDataset,
		Model:      newModel,
		BeginPhase: beginAccuracyPhase,
		Operations: []workload.Operation[*Model]{
			{Name: "compose", Weight: composeAcc, Build: action(func(m *Model) func(*workload.Rand) social.Action { return m.compose })},
			{Name: "home_read", Weight: homeAcc, Build: action(func(m *Model) func(*workload.Rand) social.Action { return m.read(true) })},
			{Name: "user_read", Weight: userAcc, Build: action(func(m *Model) func(*workload.Rand) social.Action { return m.read(false) })},
			{Name: "follow", Weight: followAcc, Build: action(func(m *Model) func(*workload.Rand) social.Action { return m.edge(social.KindFollow) })},
			{Name: "unfollow", Weight: unfollowAcc, Build: action(func(m *Model) func(*workload.Rand) social.Action { return m.edge(social.KindUnfollow) })},
			{Name: "invalid", Weight: invalidAcc, Build: action(func(m *Model) func(*workload.Rand) social.Action { return m.invalid })},
		},
	}
}
