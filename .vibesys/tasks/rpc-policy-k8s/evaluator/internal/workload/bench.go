// Package workload holds the task's two request generators on the vibesys-evaluator Go SDK:
// Bench, the benchmark's mix, and Accuracy, the accuracy check's.
package workload

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/workload"

	"rpc-policy-k8s/evaluator/internal/social"
)

// The benchmark's mix is the DeathStarBench social network task's (social-network-k8s, as its
// Go workload draws it), over this task's dataset:
//
//   - user_timeline_read (500): the user timeline of a user drawn by popularity (Zipf, s = 1,
//     over the user ids: user 1 is the most followed);
//   - home_timeline_read (400): the home timeline of a user drawn by activity (Zipf, s = 0.75,
//     over the dataset's activity order);
//   - compose_post (100): an active user posts a few words and a unique marker, a mention of
//     one of its followees 20% of the time, a URL at the end 15% and media 5%, then reads its
//     own user timeline (read your write);
//   - follow / unfollow (5 each): an active user follows a popular user it does not follow, or
//     drops one of its followees, as far as the seeded graph and this phase's changes tell.
//
// Pages are PageSize posts: the first 80% of the time, the second 12%, the third 5%, pages 4-7
// 3%.
const (
	UserTimelineWeight = 500
	HomeTimelineWeight = 400
	ComposeWeight      = 100
	FollowWeight       = 5
	UnfollowWeight     = 5

	popularExponent = 1.0
	activeExponent  = 0.75
)

// Page draws a timeline page's first entry.
func Page(rand *workload.Rand) int {
	switch x := rand.Below(100); {
	case x < 80:
		return 0
	case x < 92:
		return social.PageSize
	case x < 97:
		return 2 * social.PageSize
	default:
		return (3 + rand.Below(4)) * social.PageSize
	}
}

var words = strings.Fields(`time day way year week work home city road park river tree garden
	table chair window door book page music song game team ball field water coffee tea bread apple
	lunch market train bus bike plan idea photo color light sound paper box walk run read write cook`)

// Words are 2 to 7 words of a short list.
func Words(rand *workload.Rand) []string {
	n := 2 + rand.Below(6)
	out := make([]string, n)
	for i := range out {
		out[i] = words[rand.Below(len(words))]
	}
	return out
}

// URL is a URL of one of a million hosts.
func URL(rand *workload.Rand) string {
	return fmt.Sprintf("http://example%d.com", rand.Below(1_000_000))
}

// MediaOf is one or two media with random ids.
func MediaOf(rand *workload.Rand) []social.Media {
	n := 1 + rand.Below(2)
	out := make([]social.Media, n)
	for i := range out {
		out[i] = social.Media{ID: strconv.FormatUint(1e15+rand.Uint64()%9e15, 10), Type: []string{"png", "jpg"}[i%2]}
	}
	return out
}

func (m *Model) follows(u, v int64) bool {
	if on, ok := m.changed[[2]int64{u, v}]; ok {
		return on
	}
	return m.data.Follows(u, v)
}

func (m *Model) popular(rand *workload.Rand) int64 {
	return int64(rand.Zipf(m.data.Users, popularExponent)) + 1
}

func (m *Model) active(rand *workload.Rand) int64 {
	return m.data.Active(rand.Zipf(m.data.Users, activeExponent))
}

// Bench is the benchmark's workload. It has no info (a benchmark only measures).
func Bench() workload.Workload[*social.Dataset, *Model] {
	userTimeline := func(m *Model, rand *workload.Rand) workload.Request {
		start := Page(rand)
		return workload.Request{Steps: []workload.Step{social.ReadPage(false, m.popular(rand), start, start+social.PageSize)}}
	}
	homeTimeline := func(m *Model, rand *workload.Rand) workload.Request {
		start := Page(rand)
		return workload.Request{Steps: []workload.Step{social.ReadPage(true, m.active(rand), start, start+social.PageSize)}}
	}
	compose := func(m *Model, rand *workload.Rand) workload.Request {
		author := m.active(rand)
		parts := Words(rand)
		if rand.Random() < 0.2 {
			if f := m.data.Followees(author); len(f) > 0 {
				at := rand.Below(len(parts) + 1)
				parts = append(parts[:at], append([]string{"@" + social.Username(int64(f[rand.Below(len(f))]))}, parts[at:]...)...)
			}
		}
		parts = append(parts, "#"+m.prefix+strconv.FormatInt(m.seq, 36))
		m.seq++
		if rand.Random() < 0.15 {
			parts = append(parts, URL(rand))
		}
		var media []social.Media
		if rand.Random() < 0.05 {
			media = MediaOf(rand)
		}
		return workload.Request{Steps: []workload.Step{
			social.Compose(author, strings.Join(parts, " "), media),
			social.ReadPage(false, author, 0, social.PageSize),
		}}
	}
	edge := func(on bool) func(*Model, *workload.Rand) workload.Request {
		return func(m *Model, rand *workload.Rand) workload.Request {
			u := m.active(rand)
			var v int64
			for try := 0; try < 20; try++ {
				if on {
					v = m.popular(rand)
					if v != u && !m.follows(u, v) {
						break
					}
				} else {
					f := m.data.Followees(u)
					v = int64(f[rand.Below(len(f))])
					if m.follows(u, v) {
						break
					}
				}
			}
			m.changed[[2]int64{u, v}] = on
			return workload.Request{Steps: []workload.Step{social.Follow(u, v, on)}}
		}
	}
	return workload.Workload[*social.Dataset, *Model]{
		Dataset:    social.ReadDataset,
		Model:      newModel,
		BeginPhase: func(m *Model, phase int) error { m.seq = int64(phase) << 32; return nil },
		Operations: []workload.Operation[*Model]{
			{Name: "user_timeline_read", Weight: UserTimelineWeight, Build: userTimeline},
			{Name: "home_timeline_read", Weight: HomeTimelineWeight, Build: homeTimeline},
			{Name: "compose_post", Weight: ComposeWeight, Build: compose},
			{Name: "follow", Weight: FollowWeight, Build: edge(true)},
			{Name: "unfollow", Weight: UnfollowWeight, Build: edge(false)},
		},
	}
}
