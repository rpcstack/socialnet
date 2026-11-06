package oracle

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"testing"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/check"

	"rpc-policy-k8s/evaluator/internal/social"
)

// fake is the application as the README describes it, run one request at a time (3 ms each),
// recording the history. A test sets a bug to see the oracle fail it.
type fake struct {
	data  *social.Dataset
	calls []check.Call
	now   int64 // ns
	edges map[[2]int64]bool
	user  map[int64][]*social.Post // newest first
	home  map[int64][]*social.Post
	seq   int
	bug   func(f *fake, a social.Action, ans []*social.Post) []*social.Post
}

func newFake() *fake {
	return &fake{data: social.New(7, 200, 12), now: 1_000_000_000, edges: map[[2]int64]bool{},
		user: map[int64][]*social.Post{}, home: map[int64][]*social.Post{}}
}

func (f *fake) follows(u, v int64) bool {
	if on, ok := f.edges[[2]int64{u, v}]; ok {
		return on
	}
	return f.data.Follows(u, v)
}

func (f *fake) record(a social.Action, status int, body string) {
	raw, _ := json.Marshal(a)
	op := map[string]string{social.KindCompose: "compose", social.KindFollow: "follow", social.KindUnfollow: "unfollow", social.KindRead: "user_read"}[a.Kind]
	if a.Home {
		op = "home_read"
	}
	start := f.now
	f.now += 10_000_000
	f.calls = append(f.calls, check.Call{Index: uint64(len(f.calls)), Op: op, Info: raw, Start: start, End: start + 3_000_000,
		Steps: []check.Step{{Start: start, End: start + 3_000_000, Status: status, Body: []byte(body)}}})
}

func (f *fake) compose(u, mention int64) string {
	marker := fmt.Sprintf("mt%d", f.seq)
	f.seq++
	a := social.Action{Kind: social.KindCompose, User: u, Marker: marker, Mention: mention}
	text := "word word #" + marker
	if mention != 0 {
		text = "@" + social.Username(mention) + " " + text
	}
	a.Text = text
	ts := (f.now + 1_500_000) / 1_000_000
	p := &social.Post{PostID: strconv.Itoa(1000 + f.seq), Creator: social.Creator{UserID: strconv.FormatInt(u, 10), Username: social.Username(u)},
		ReqID: "1", Text: text, Mentions: []social.Mention{}, Media: []social.Media{}, URLs: []social.URL{}, Timestamp: strconv.FormatInt(ts, 10), TS: ts}
	if mention != 0 {
		p.Text = strings.Replace(text, "@"+social.Username(mention), "user_id:"+strconv.FormatInt(mention, 10), 1)
		p.Mentions = []social.Mention{{UserID: strconv.FormatInt(mention, 10), Username: social.Username(mention) + "@mongodb"}}
	}
	f.user[u] = append([]*social.Post{p}, f.user[u]...)
	for r := int64(1); r <= int64(f.data.Users); r++ {
		if r != u && (f.follows(r, u) || r == mention) {
			f.home[r] = append([]*social.Post{p}, f.home[r]...)
		}
	}
	f.record(a, 200, social.MsgComposed)
	return marker
}

func (f *fake) edge(u, v int64, on bool) {
	kind := social.KindUnfollow
	if on {
		kind = social.KindFollow
	}
	f.edges[[2]int64{u, v}] = on
	f.record(social.Action{Kind: kind, User: u, Target: v}, 200, social.MsgSuccess)
}

func (f *fake) read(u int64, home bool, start int) {
	src := f.user[u]
	if home {
		src = f.home[u]
	}
	var ans []*social.Post
	for i := start; i < start+social.PageSize && i < len(src); i++ {
		ans = append(ans, src[i])
	}
	a := social.Action{Kind: social.KindRead, User: u, Home: home, Start: start, Stop: start + social.PageSize}
	if f.bug != nil {
		ans = f.bug(f, a, ans)
	}
	body := []byte("[]\n")
	if len(ans) > 0 {
		body, _ = json.Marshal(ans)
		body = append(body, '\n')
	}
	f.record(a, 200, string(body))
}

func (f *fake) check(t *testing.T) *check.Report { return f.checkWith(t, check.Options{}) }

func (f *fake) checkWith(t *testing.T, o check.Options) *check.Report {
	t.Helper()
	h := check.NewHistory([][]check.Call{f.calls}, nil)
	facts, err := Collect(f.data, phasesOf(h))
	if err != nil {
		t.Fatal(err)
	}
	return check.Check(h, []check.AnySpec{Author(facts), Reader(facts), Responses(facts)}, o)
}

// run is a scripted history on hot users 1..12: composes, follow changes and reads, with the
// mentions and the edges the specs must follow.
func (f *fake) run() {
	hot := int64(f.data.Hot)
	for round := 0; round < 12; round++ {
		for u := int64(1); u <= hot; u++ {
			switch (int(u) + round) % 5 {
			case 0, 1:
				var m int64
				if round%3 == 0 {
					m = u%hot + 1
				}
				f.compose(u, m)
			case 2:
				f.read(u, true, 0)
			case 3:
				f.read(u, false, 0)
				f.read(u, true, social.PageSize*(round%3))
			case 4:
				v := (u+int64(round))%hot + 1
				if v != u && !f.data.Follows(u, v) {
					f.edge(u, v, round%2 == 0)
				}
			}
		}
	}
	for u := int64(1); u <= hot; u++ {
		f.read(u, true, 0)
		f.read(u, false, 0)
	}
}

func expectVerdict(t *testing.T, r *check.Report, want check.Verdict, text string) {
	t.Helper()
	out := r.Text(10)
	if r.Verdict != want || !strings.Contains(out, text) {
		t.Fatalf("verdict %s, want %s with %q:\n%s", r.Verdict, want, text, out)
	}
}

func TestCorrectHistoryPasses(t *testing.T) {
	f := newFake()
	f.run()
	r := f.check(t)
	expectVerdict(t, r, check.Pass, "")
	var out strings.Builder
	if err := Stats(check.NewHistory([][]check.Call{f.calls}, nil), &out); err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"all: compose", "all: home_read", "served at least once"} {
		if !strings.Contains(out.String(), want) {
			t.Errorf("stats lack %q:\n%s", want, out.String())
		}
	}
}

// bug runs the script with the bug in and expects FAIL with the text.
func bug(t *testing.T, text string, b func(f *fake, a social.Action, ans []*social.Post) []*social.Post) {
	t.Helper()
	f := newFake()
	f.bug = b
	f.run()
	expectVerdict(t, f.check(t), check.Fail, text)
}

func TestMissingNewestHomePostFails(t *testing.T) {
	n := 0
	bug(t, "newer", func(f *fake, a social.Action, ans []*social.Post) []*social.Post {
		if a.Home && a.Start == 0 && len(ans) > 2 {
			if n++; n == 3 {
				return ans[1:] // the newest post is not there
			}
		}
		return ans
	})
}

func TestStaleUserTimelineFails(t *testing.T) {
	var last []*social.Post
	bug(t, "", func(f *fake, a social.Action, ans []*social.Post) []*social.Post {
		if a.Home || a.Start != 0 {
			return ans
		}
		if last != nil && len(ans) > 0 && len(last) > 0 && last[0] != ans[0] {
			ans, last = last, nil // a cached page, once
			return ans
		}
		last = ans
		return ans
	})
}

func TestDuplicatedPostFails(t *testing.T) {
	bug(t, "twice", func(f *fake, a social.Action, ans []*social.Post) []*social.Post {
		if !a.Home && len(ans) > 1 {
			return append([]*social.Post{ans[0]}, ans...)
		}
		return ans
	})
}

func TestPostOfAnUnfollowedAuthorFails(t *testing.T) {
	// A home timeline that shows a post by a user the reader never followed.
	bug(t, "not on the timeline", func(f *fake, a social.Action, ans []*social.Post) []*social.Post {
		if a.Home && a.Start == 0 && len(ans) > 0 {
			for u := int64(1); u <= int64(f.data.Hot); u++ {
				if u != a.User && !f.follows(a.User, u) && len(f.user[u]) > 0 {
					p := f.user[u][0]
					if p.TS > ans[0].TS {
						return append([]*social.Post{p}, ans[:len(ans)-1]...)
					}
				}
			}
		}
		return ans
	})
}

func TestShortPageFails(t *testing.T) {
	bug(t, "", func(f *fake, a social.Action, ans []*social.Post) []*social.Post {
		if a.Home && a.Start == 0 && len(ans) == social.PageSize {
			return ans[:5]
		}
		return ans
	})
}

func TestAlteredTextFails(t *testing.T) {
	bug(t, "text", func(f *fake, a social.Action, ans []*social.Post) []*social.Post {
		if !a.Home && len(ans) > 0 {
			q := *ans[0]
			q.Text = strings.Replace(q.Text, "word", "wurd", 1)
			return append([]*social.Post{&q}, ans[1:]...)
		}
		return ans
	})
}

func TestLostResponsesAreUnknown(t *testing.T) {
	f := newFake()
	f.run()
	// One compose whose answer was lost: it may or may not have happened, the check passes.
	for i := range f.calls {
		if f.calls[i].Op == "compose" {
			f.calls[i].Steps[0].Status = 0
			f.calls[i].Steps[0].Error = "timeout"
			f.calls[i].Steps[0].Body = nil
			break
		}
	}
	r := f.checkWith(t, check.Options{MaxLostShare: 0.1})
	if r.Verdict == check.Fail {
		t.Fatalf("a lost compose failed the check:\n%s", r.Text(10))
	}
}
