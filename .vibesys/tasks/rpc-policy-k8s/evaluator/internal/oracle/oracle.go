// Package oracle is the task's accuracy oracle: the specs a captured history of the accuracy
// workload must hold (package check of vibesys-evaluator).
//
// The application, as it is (README.md of the repository, "Where the Rust services behind it
// differ"): compose-post stores the post, then writes the author's user timeline and, in the
// same request, the home timelines of the author's followers and of the users the post
// mentions (the author's own is not written). Timelines are sorted sets scored by the post's
// timestamp, and the first page of a timeline of a few posts is read from Redis, the rest from
// MongoDB, so a restart or a flush of a cache changes nothing. There is no staleness: a post
// acknowledged before a read started is on the timelines it belongs to.
//
// Three specs over one history, all of which must hold:
//
//   - "author" has one key per hot author ("a:<user>"): the author's composes and the reads of
//     the author's user timeline. The state is the set of the author's posts, and a read must
//     show exactly its window of them, newest first.
//   - "reader" has one key per hot reader ("r:<user>"): the composes that can reach the reader
//     (by a followee in the seeded graph or after a follow change of the reader's, or mentioning
//     the reader), the reader's follow changes and the reads of the reader's home timeline. The
//     state is the reader's follow edges and the set of posts delivered to it: a compose
//     delivers its post when it linearizes while the reader follows the author (or when it
//     mentions the reader), so a follow that returned before a compose started delivers it, an
//     unfollow that returned before does not, and overlapping ones may do either. A read must
//     show exactly its window of the delivered posts, newest first.
//   - "responses" checks every answer on its own (status, body, each post's content and
//     timestamp, the order of a page, the invalid requests) and counts every call once toward
//     the share of unknown outcomes the check allows.
//
// A page is a window of a list sorted by timestamp, and the timestamp is taken by the
// application inside the compose: the oracle learns it from the first time a post is served and
// bounds it by the compose's call window until then (facts.go). So a window is judged in
// intervals: the posts that certainly precede the page's first entry are at most its start, the
// posts that may precede it at least; no post certainly sorts inside the page; a short page is
// the end of the list (checkPage).
package oracle

import (
	"fmt"
	"iter"
	"strconv"
	"strings"
	"time"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/check"

	"rpc-policy-k8s/evaluator/internal/social"
)

// EffectWindow bounds how long after its call a request whose outcome is unknown may still take
// effect. A 5xx is an answer: the request ended before it, and only what it had started in
// other services can still land, within a moment. A lost answer (a timeout, a reset) ended the
// call when loadgen gave up, so its window starts there. Unknown outcomes are the cost of a
// search (the evaluator docs, "Unknown outcomes and EffectWindow"): the longer one stays open,
// the more of the history it overlaps.
const EffectWindow = 3 * time.Second

// Slack bounds the difference between the application's clock and the client's when a post's
// timestamp is compared with the time of the compose (the cluster's nodes share a kernel's
// clock; this is for rounding and for the phase pods' own clocks).
const Slack = 500 * time.Millisecond

// Specs is the oracle: the specs a history must hold, over the dataset the generator read.
func Specs(h *check.History) ([]check.AnySpec, error) {
	data := social.Default()
	if h.Dataset != "" {
		var err error
		if data, err = social.ReadDataset(h.Dataset); err != nil {
			return nil, err
		}
	}
	f, err := Collect(data, phasesOf(h))
	if err != nil {
		return nil, err
	}
	return []check.AnySpec{Author(f), Reader(f), Responses(f)}, nil
}

// ------------------------------------------------------------------ the model

// Piece kinds.
const (
	kCompose = iota + 1
	kEdge
	kRead
)

// In is a piece of the author and reader specs. Out is not used: what a read answered is in
// Page.
type In struct {
	Kind int8
	// Compose: the compose's index in the facts. Mention: the post mentions the reader.
	CI      int32
	Mention bool
	// Edge: the followee the reader's change is of, and whether it follows.
	Followee int64
	On       bool
	// Read: the window and the page, as compose indices.
	Start, Stop int
	Page        []int32
}

type node struct {
	ci   int32
	next *node
	n    int
	sum  uint64
}

func (n *node) count() int {
	if n == nil {
		return 0
	}
	return n.n
}

func (n *node) sumOf() uint64 {
	if n == nil {
		return 0
	}
	return n.sum
}

func (n *node) with(ci int32) *node {
	return &node{ci: ci, next: n, n: n.count() + 1, sum: n.sumOf() + mix(uint64(ci))}
}

func (n *node) has(ci int32) bool {
	for ; n != nil; n = n.next {
		if n.ci == ci {
			return true
		}
	}
	return false
}

// without is n with ci removed (n itself when it is not there).
func (n *node) without(ci int32) *node {
	if !n.has(ci) {
		return n
	}
	var keep []int32
	for p := n; p != nil; p = p.next {
		if p.ci != ci {
			keep = append(keep, p.ci)
		}
	}
	var out *node
	for i := len(keep) - 1; i >= 0; i-- {
		out = out.with(keep[i])
	}
	return out
}

// State is the posts on a timeline (a persistent list, so that a step that adds none copies
// nothing): those that are there (head), and those whose compose had an unknown outcome and that
// no read has shown yet, which may or may not be (maybe: kept apart, not branched into states,
// so that a burst of failed composes does not multiply the search; a read that shows one makes
// it certain). For a reader it also holds the hot users it follows (bit v-1).
type State struct {
	head, maybe *node
	edges       uint64
}

func (s State) count() int { return s.head.count() }

func (s State) with(ci int32) State      { s.head = s.head.with(ci); return s }
func (s State) withMaybe(ci int32) State { s.maybe = s.maybe.with(ci); return s }

func mix(x uint64) uint64 {
	x += 0x9e3779b97f4a7c15
	x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9
	x = (x ^ (x >> 27)) * 0x94d049bb133111eb
	return x ^ (x >> 31)
}

func (s State) equal(o State) bool {
	return s.edges == o.edges && s.head.count() == o.head.count() && s.head.sumOf() == o.head.sumOf() &&
		s.maybe.count() == o.maybe.count() && s.maybe.sumOf() == o.maybe.sumOf()
}
func (s State) hash() uint64 {
	return s.head.sumOf() ^ mix(s.edges+uint64(s.head.count())) ^ mix(s.maybe.sumOf()+7)
}

func (s State) has(ci int32) bool { return s.head.has(ci) || s.maybe.has(ci) }

func hotBit(id int64) uint64 { return 1 << (id - 1) }

// checkPage says what is wrong with a read of the window [start, stop) that showed page, over a
// timeline holding the posts of s, or "". The posts' timestamps (ms) are bounded by f.lo and
// f.hi (exact for a post that was served). Posts that may be there (maybe) count only where
// the page could not tell.
func checkPage(f *Facts, s State, page []int32, start, stop int) string {
	want := stop - start
	if len(page) == 0 {
		if s.count() > start {
			return fmt.Sprintf("an empty page for [%d, %d), but %d posts are on the timeline", start, stop, s.count())
		}
		return ""
	}
	for _, ci := range page {
		if !s.has(ci) {
			return fmt.Sprintf("it shows post %s (%s), which is not on the timeline (not delivered, or not yet composed)",
				f.posts[ci].marker, f.posts[ci].what.Text)
		}
	}
	t0, tl := f.lo[page[0]], f.lo[page[len(page)-1]]
	var dmin, dmax, other, otherMaybe int
	var gap int32 = -1
	for n := s.head; n != nil; n = n.next {
		ci := n.ci
		if inPage(page, ci) {
			continue
		}
		other++
		if f.lo[ci] > t0 {
			dmin++
		}
		if f.hi[ci] >= t0 {
			dmax++
		}
		if f.lo[ci] > tl && f.hi[ci] < t0 {
			gap = ci
		}
	}
	for n := s.maybe; n != nil; n = n.next {
		if inPage(page, n.ci) {
			continue
		}
		otherMaybe++
		if f.hi[n.ci] >= t0 {
			dmax++
		}
	}
	if gap >= 0 {
		p := &f.posts[gap]
		return fmt.Sprintf("post %s by %d (composed at %s) is on the timeline and sorts inside the page (%d..%d ms), but the page does not show it",
			p.marker, p.author, f.seen(gap), tl, t0)
	}
	if start < dmin {
		return fmt.Sprintf("the page starts at entry %d, but at least %d posts are newer than its first post", start, dmin)
	}
	if start > dmax {
		return fmt.Sprintf("the page starts at entry %d, but at most %d posts are newer than its first post (the page begins too early)", start, dmax)
	}
	if len(page) < want && (other > start || start > other+otherMaybe) {
		return fmt.Sprintf("a short page (%d of %d entries) is the end of the timeline, which then has %d posts; %d are on it", len(page), want, start+len(page), other+len(page))
	}
	return ""
}

func inPage(page []int32, ci int32) bool {
	for _, p := range page {
		if p == ci {
			return true
		}
	}
	return false
}

// seen describes compose ci for a report.
func (f *Facts) seen(ci int32) string {
	p := &f.posts[ci]
	if p.served != nil {
		return "first served by " + p.seenAt
	}
	return fmt.Sprintf("op %d of phase %d, never served", p.index, p.phase)
}

func (f *Facts) stepRead(s State, in In) ([]State, string) {
	if why := checkPage(f, s, in.Page, in.Start, in.Stop); why != "" {
		return nil, why
	}
	for _, ci := range in.Page {
		if s.maybe.has(ci) { // shown, so it took effect
			s.maybe = s.maybe.without(ci)
			s.head = s.head.with(ci)
		}
	}
	return []State{s}, ""
}

// ------------------------------------------------------------------ author

// Author is the author spec.
func Author(f *Facts) *check.Spec[State, In, struct{}] {
	return &check.Spec[State, In, struct{}]{
		Name: "author",
		Project: func(phases iter.Seq[[]check.Call], out *check.Projection[In, struct{}]) error {
			return calls(phases, func(ac *acall) error {
				a := ac.a
				switch {
				case a.Kind == social.KindCompose && ac.valid():
					ci := f.byMarker[a.Marker]
					p := &f.posts[ci]
					if !ac.unknown && !p.acked {
						return nil // the responses spec reports it
					}
					out.Add(check.Piece[In, struct{}]{Keys: []string{akey(a.User)}, In: In{Kind: kCompose, CI: ci}, Write: true, Unknown: ac.unknown, Origin: ac.c})
				case a.Kind == social.KindRead && !a.Home && ac.valid():
					if page, ok := pageOf(f, ac); ok {
						out.Add(check.Piece[In, struct{}]{Keys: []string{akey(a.User)}, In: In{Kind: kRead, Start: a.Start, Stop: a.Stop, Page: page}, Origin: ac.c})
					}
				}
				return nil
			})
		},
		Init: func([]string) State { return State{} },
		Step: func(s State, op check.Op[In, struct{}]) ([]State, string) {
			if op.In.Kind == kCompose {
				return composeStep(s, op.In.CI, true, op.Unknown)
			}
			return f.stepRead(s, op.In)
		},
		Equal:         State.equal,
		Hash:          State.hash,
		EffectWindow:  EffectWindow,
		DescribeState: func(s State) string { return fmt.Sprintf("%d posts", s.count()) },
		DescribeOp:    describe(f),
	}
}

// composeStep adds post ci when it is delivered; an unknown compose may not have been.
func composeStep(s State, ci int32, delivered, unknown bool) ([]State, string) {
	switch {
	case !delivered:
		return []State{s}, ""
	case unknown:
		return []State{s.withMaybe(ci)}, ""
	}
	return []State{s.with(ci)}, ""
}

func akey(u int64) string { return "a:" + strconv.FormatInt(u, 10) }
func rkey(u int64) string { return "r:" + strconv.FormatInt(u, 10) }

// pageOf is a valid read's page as compose indices, when it is a 200 that parses and shows only
// posts of the workload (the responses spec reports the others).
func pageOf(f *Facts, ac *acall) ([]int32, bool) {
	posts, ok := ac.page()
	if !ok {
		return nil, false
	}
	idx, ok := f.match(posts)
	return idx, ok
}

// ------------------------------------------------------------------ reader

// Reader is the reader spec.
func Reader(f *Facts) *check.Spec[State, In, struct{}] {
	data := f.data
	return &check.Spec[State, In, struct{}]{
		Name: "reader",
		Project: func(phases iter.Seq[[]check.Call], out *check.Projection[In, struct{}]) error {
			byAuthor := map[int64]uint64{} // follow-based recipients, by author
			for a := int64(1); a <= int64(data.Hot); a++ {
				for r := range f.readers {
					if r != a && (data.Follows(r, a) || f.changes[r]&hotBit(a) != 0) {
						byAuthor[a] |= hotBit(r)
					}
				}
			}
			return calls(phases, func(ac *acall) error {
				a := ac.a
				switch {
				case a.Kind == social.KindCompose && ac.valid():
					ci := f.byMarker[a.Marker]
					p := &f.posts[ci]
					if !ac.unknown && !p.acked {
						return nil
					}
					set := byAuthor[a.User]
					if m := a.Mention; m != 0 && m != a.User && f.readers[m] {
						set |= hotBit(m)
					}
					for r := int64(1); r <= int64(data.Hot); r++ {
						if set&hotBit(r) != 0 {
							out.Add(check.Piece[In, struct{}]{Keys: []string{rkey(r)}, In: In{Kind: kCompose, CI: ci, Mention: a.Mention == r},
								Write: true, Unknown: ac.unknown, Origin: ac.c})
						}
					}
				case (a.Kind == social.KindFollow || a.Kind == social.KindUnfollow) && ac.valid():
					if !f.readers[a.User] {
						return nil
					}
					msg := ""
					if !ac.unknown {
						msg = string(ac.body())
						if ac.status() != 200 || msg != social.MsgSuccess {
							return nil
						}
					}
					out.Add(check.Piece[In, struct{}]{Keys: []string{rkey(a.User)}, In: In{Kind: kEdge, Followee: a.Target, On: a.Kind == social.KindFollow},
						Write: true, Unknown: ac.unknown, Origin: ac.c})
				case a.Kind == social.KindRead && a.Home && ac.valid() && f.readers[a.User]:
					if page, ok := pageOf(f, ac); ok {
						out.Add(check.Piece[In, struct{}]{Keys: []string{rkey(a.User)}, In: In{Kind: kRead, Start: a.Start, Stop: a.Stop, Page: page}, Origin: ac.c})
					}
				}
				return nil
			})
		},
		Init: func(keys []string) State {
			r, _ := strconv.ParseInt(strings.TrimPrefix(keys[0], "r:"), 10, 64)
			return State{edges: data.HotFollowees(r)}
		},
		Step: func(s State, op check.Op[In, struct{}]) ([]State, string) {
			in := op.In
			switch in.Kind {
			case kCompose:
				author := f.posts[in.CI].author
				return composeStep(s, in.CI, in.Mention || s.edges&hotBit(author) != 0, op.Unknown)
			case kEdge:
				n := s
				if in.On {
					n.edges |= hotBit(in.Followee)
				} else {
					n.edges &^= hotBit(in.Followee)
				}
				if op.Unknown {
					return []State{s, n}, ""
				}
				return []State{n}, ""
			}
			return f.stepRead(s, in)
		},
		Equal:         State.equal,
		Hash:          State.hash,
		EffectWindow:  EffectWindow,
		DescribeState: func(s State) string { return fmt.Sprintf("%d posts, follows %b", s.count(), s.edges) },
		DescribeOp:    describe(f),
	}
}

func describe(f *Facts) func(op check.Op[In, struct{}]) string {
	return func(op check.Op[In, struct{}]) string {
		in := op.In
		res := ""
		if op.Unknown {
			res = " (outcome unknown)"
		}
		switch in.Kind {
		case kCompose:
			p := &f.posts[in.CI]
			return fmt.Sprintf("compose #%s by %d%s", p.marker, p.author, res)
		case kEdge:
			return fmt.Sprintf("%s %d%s", map[bool]string{true: "follow", false: "unfollow"}[in.On], in.Followee, res)
		}
		var ms []string
		for _, ci := range in.Page {
			ms = append(ms, tail(f.posts[ci].marker))
		}
		return fmt.Sprintf("read [%d, %d): %d posts (%s)", in.Start, in.Stop, len(in.Page), strings.Join(ms, " "))
	}
}

// ------------------------------------------------------------------ responses

// Responses checks every answer on its own and counts every call once: unknown when its answer
// was lost or a 5xx.
func Responses(f *Facts) *check.Spec[struct{}, struct{}, struct{}] {
	return &check.Spec[struct{}, struct{}, struct{}]{
		Name: "responses",
		Project: func(phases iter.Seq[[]check.Call], out *check.Projection[struct{}, struct{}]) error {
			return calls(phases, func(ac *acall) error {
				out.Add(check.Piece[struct{}, struct{}]{Unknown: ac.unknown, Origin: ac.c})
				if ac.unknown {
					return nil
				}
				if problem := Judge(f, ac); problem != "" {
					out.Violation(ac.c, "%s", problem)
				}
				return nil
			})
		},
		Init:  func([]string) struct{} { return struct{}{} },
		Step:  func(s struct{}, _ check.Op[struct{}, struct{}]) ([]struct{}, string) { return []struct{}{s}, "" },
		Equal: func(struct{}, struct{}) bool { return true },
		Hash:  func(struct{}) uint64 { return 0 },
	}
}

func expect(ac *acall, status int, body string) string {
	if ac.status() != status || string(ac.body()) != body {
		return fmt.Sprintf("status %d, %q; want %d, %q", ac.status(), snippet(ac.body()), status, body)
	}
	return ""
}

// Judge returns what is wrong with an answer on its own, or "".
func Judge(f *Facts, ac *acall) string {
	a := ac.a
	switch a.Kind {
	case social.KindCompose:
		if !ac.valid() {
			return expect(ac, 400, social.MsgIncomplete)
		}
		return expect(ac, 200, social.MsgComposed)
	case social.KindFollow, social.KindUnfollow:
		if !ac.valid() {
			return expect(ac, 400, social.MsgIncomplete)
		}
		return expect(ac, 200, social.MsgSuccess)
	}
	// reads
	switch a.Invalid {
	case social.InvalidUser:
		if ac.status() != 400 {
			return fmt.Sprintf("a read of user %q: status %d, %q; want 400", "abc", ac.status(), snippet(ac.body()))
		}
		return ""
	case social.InvalidEmptyWindow, social.InvalidNegative:
		return expect(ac, 200, "[]\n")
	}
	if ac.status() != 200 {
		return fmt.Sprintf("status %d (%s); want 200", ac.status(), snippet(ac.body()))
	}
	posts, err := social.ParseTimeline(ac.body())
	if err != nil {
		return err.Error()
	}
	if len(posts) > a.Stop-a.Start {
		return fmt.Sprintf("%d posts for the window [%d, %d)", len(posts), a.Start, a.Stop)
	}
	seen := map[int32]bool{}
	for i := range posts {
		p := &posts[i]
		ci, ok := f.byMarker[social.MarkerOf(p.Text)]
		if !ok {
			return fmt.Sprintf("post %s %q is not one the workload composed", p.PostID, p.Text)
		}
		c := &f.posts[ci]
		if seen[ci] {
			return fmt.Sprintf("post #%s (by %d) appears twice on the page", c.marker, c.author)
		}
		seen[ci] = true
		if why := c.what.Check(p); why != "" {
			return fmt.Sprintf("post #%s: %s", c.marker, why)
		}
		if a.Home == (c.author == a.User) {
			if a.Home {
				return fmt.Sprintf("the home timeline of %d shows its own post #%s (the author's is not written)", a.User, c.marker)
			}
			return fmt.Sprintf("the user timeline of %d shows a post #%s by %d", a.User, c.marker, c.author)
		}
		if c.served != nil && p.TS != c.served.TS || p.PostID != postID(f, ci, p) {
			return fmt.Sprintf("post #%s is served with timestamp %d and id %s, but was first served (%s) with %d and %s",
				c.marker, p.TS, p.PostID, c.seenAt, c.served.TS, c.served.PostID)
		}
		if p.TS < f.wlo[ci] || p.TS > f.whi[ci] {
			return fmt.Sprintf("post #%s has timestamp %d ms, but its compose was called at %d..%d ms", c.marker, p.TS, f.wlo[ci], f.whi[ci])
		}
		// A user timeline merges Redis with MongoDB, which lists posts in the order they were
		// written, not by timestamp: a slow compose followed by a fast one of the same author can
		// swap two entries there. The home timeline is Redis alone.
		if a.Home && i > 0 && p.TS > posts[i-1].TS {
			return fmt.Sprintf("the page is not newest first: post #%s (%d) follows %d", c.marker, p.TS, posts[i-1].TS)
		}
		if c.start > ac.c.End {
			return fmt.Sprintf("post #%s is served before its compose was called", c.marker)
		}
	}
	return ""
}

// postID is the id post ci was first served with, or p's own when it was not served before.
func postID(f *Facts, ci int32, p *social.Post) string {
	if s := f.posts[ci].served; s != nil {
		return s.PostID
	}
	return p.PostID
}

func snippet(b []byte) string {
	s := strings.TrimSpace(string(b))
	if len(s) > 200 {
		s = s[:200] + "..."
	}
	return strconv.Quote(s)
}

// tail is the end of a marker, enough to tell posts apart in a report.
func tail(marker string) string {
	if len(marker) > 5 {
		return marker[len(marker)-5:]
	}
	return marker
}
