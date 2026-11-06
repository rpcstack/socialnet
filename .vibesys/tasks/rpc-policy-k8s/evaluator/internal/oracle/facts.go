package oracle

import (
	"bytes"
	"encoding/json"
	"fmt"
	"iter"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/check"

	"rpc-policy-k8s/evaluator/internal/social"
)

// post is one compose of the history, as the specs need it after the first pass.
type post struct {
	phase  int
	index  uint64
	author int64
	marker string
	what   *social.Composed
	// start and end are the compose's call times (ns, the loadgen clock); unknown when its outcome
	// is (a lost answer or a 5xx: it may have taken effect within EffectWindow, or never).
	start, end int64
	unknown    bool
	acked      bool
	// served is its first sighting in a timeline (nil: never served), seenAt the call that served
	// it.
	served *social.Post
	seenAt string
}

// Facts are what the projections must know about the whole history before they add a piece:
// every compose (by its index in the history) with the post it made as first served, which hot
// readers follow or unfollow whom, and which hot users read their home timelines. They are
// collected once, in a first pass over the phases, and shared by the specs.
type Facts struct {
	data     *social.Dataset
	posts    []post
	byMarker map[string]int32
	// changes[r] is the set (bit v-1) of hot users r sends follow changes for.
	changes map[int64]uint64
	readers map[int64]bool
	// lo and hi are bounds(i) of every compose, in ms; wlo and whi bound the timestamp a post of
	// the compose may have whatever was served (its call window with the clock slack).
	lo, hi, wlo, whi []int64
}

// acall is one call decoded: its action and whether its outcome is unknown.
type acall struct {
	c       *check.Call
	a       social.Action
	unknown bool
}

func decode(c *check.Call) (acall, error) {
	ac := acall{c: c}
	dec := json.NewDecoder(bytes.NewReader(c.Info))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&ac.a); err != nil {
		return ac, fmt.Errorf("%s %d of phase %d: its info %.200q: %w (a task bug)", c.Op, c.Index, c.Phase, c.Info, err)
	}
	switch ac.a.Kind {
	case social.KindCompose, social.KindFollow, social.KindUnfollow, social.KindRead:
	default:
		return ac, fmt.Errorf("%s %d of phase %d: unknown action %q (a task bug)", c.Op, c.Index, c.Phase, ac.a.Kind)
	}
	if len(c.Steps) != 1 && !c.Lost() {
		return ac, fmt.Errorf("%s %d of phase %d: %d steps, the workload sends one (a task bug)", c.Op, c.Index, c.Phase, len(c.Steps))
	}
	ac.unknown = unknownOutcome(c)
	return ac, nil
}

// unknownOutcome says whether a call's outcome is unknown: no answer, or a 5xx.
func unknownOutcome(c *check.Call) bool {
	if c.Lost() || len(c.Steps) == 0 {
		return true
	}
	for _, s := range c.Steps {
		if s.Status >= 500 || s.Status == 0 {
			return true
		}
	}
	return false
}

func (ac *acall) status() int  { return ac.c.Steps[0].Status }
func (ac *acall) body() []byte { return ac.c.Steps[0].Body }

// valid says whether the call is a well-formed request of the workload (not one of its invalid
// ones).
func (ac *acall) valid() bool { return ac.a.Invalid == "" }

// page is a valid read's answer: its posts, when it is a 200 with a timeline that parses.
func (ac *acall) page() ([]social.Post, bool) {
	if ac.unknown || ac.status() != 200 {
		return nil, false
	}
	posts, err := social.ParseTimeline(ac.body())
	return posts, err == nil
}

func calls(phases iter.Seq[[]check.Call], each func(*acall) error) error {
	for cs := range phases {
		for i := range cs {
			ac, err := decode(&cs[i])
			if err != nil {
				return err
			}
			if err := each(&ac); err != nil {
				return err
			}
		}
	}
	return nil
}

func phasesOf(h *check.History) iter.Seq[[]check.Call] {
	return func(yield func([]check.Call) bool) {
		for p := 0; p < h.Phases; p++ {
			calls, err := h.Phase(p)
			if err != nil {
				panic(phaseError{err})
			}
			if !yield(calls) {
				return
			}
		}
	}
}

type phaseError struct{ err error }

// Collect is the first pass: every compose, its first sighting, the follow changes and the
// readers.
func Collect(data *social.Dataset, phases iter.Seq[[]check.Call]) (f *Facts, err error) {
	defer func() {
		if r := recover(); r != nil {
			pe, ok := r.(phaseError)
			if !ok {
				panic(r)
			}
			err = pe.err
		}
	}()
	f = &Facts{data: data, byMarker: map[string]int32{}, changes: map[int64]uint64{}, readers: map[int64]bool{}}
	// Composes first, so that a page can be matched to a compose of a later phase too (which
	// would be wrong, and is reported as such).
	err = calls(phases, func(ac *acall) error {
		a := ac.a
		if a.Kind != social.KindCompose || !ac.valid() {
			return nil
		}
		if a.Marker == "" || !data.IsHot(a.User) {
			return fmt.Errorf("compose %d of phase %d has no marker or is by user %d, not a hot one (a task bug)", ac.c.Index, ac.c.Phase, a.User)
		}
		if _, dup := f.byMarker[a.Marker]; dup {
			return fmt.Errorf("two composes have the marker %s (a task bug)", a.Marker)
		}
		acked := !ac.unknown && ac.status() == 200 && string(ac.body()) == social.MsgComposed
		f.byMarker[a.Marker] = int32(len(f.posts))
		f.posts = append(f.posts, post{
			phase: ac.c.Phase, index: ac.c.Index, author: a.User, marker: a.Marker, what: a.Composed(),
			start: ac.c.Start, end: ac.c.End, unknown: ac.unknown, acked: acked,
		})
		return nil
	})
	if err != nil {
		return nil, err
	}
	err = calls(phases, func(ac *acall) error {
		a := ac.a
		switch {
		case (a.Kind == social.KindFollow || a.Kind == social.KindUnfollow) && ac.valid():
			if !data.IsHot(a.User) || !data.IsHot(a.Target) {
				return fmt.Errorf("%s %d of phase %d is of users %d -> %d, not hot ones (a task bug)", a.Kind, ac.c.Index, ac.c.Phase, a.User, a.Target)
			}
			f.changes[a.User] |= 1 << (a.Target - 1)
		case a.Kind == social.KindRead && ac.valid():
			if a.Home && data.IsHot(a.User) {
				f.readers[a.User] = true
			}
			page, ok := ac.page()
			if !ok {
				return nil
			}
			for i := range page {
				ci, ok := f.byMarker[social.MarkerOf(page[i].Text)]
				if ok && f.posts[ci].served == nil {
					p := page[i]
					f.posts[ci].served = &p
					f.posts[ci].seenAt = fmt.Sprintf("%s %d of phase %d", ac.c.Op, ac.c.Index, ac.c.Phase)
				}
			}
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	f.lo, f.hi = make([]int64, len(f.posts)), make([]int64, len(f.posts))
	f.wlo, f.whi = make([]int64, len(f.posts)), make([]int64, len(f.posts))
	for i := range f.posts {
		f.lo[i], f.hi[i] = f.bounds(int32(i))
		p := &f.posts[i]
		end := p.end
		if p.unknown {
			end = p.start + int64(EffectWindow)
		}
		f.wlo[i], f.whi[i] = (p.start-int64(Slack))/1e6, (end+int64(Slack))/1e6
	}
	return f, nil
}

// match maps a valid read's page to compose indices: ok is false when a post is not one the
// workload composed (the calls spec reports it).
func (f *Facts) match(page []social.Post) ([]int32, bool) {
	out := make([]int32, len(page))
	for i := range page {
		ci, ok := f.byMarker[social.MarkerOf(page[i].Text)]
		if !ok {
			return nil, false
		}
		out[i] = ci
	}
	return out, true
}

// bounds is the least and most timestamp (ms) compose ci's post may have: the one it was served
// with, or its compose's call window with the clock slack.
func (f *Facts) bounds(ci int32) (lo, hi int64) {
	p := &f.posts[ci]
	if p.served != nil {
		return p.served.TS, p.served.TS
	}
	end := p.end
	if p.unknown {
		end = p.start + int64(EffectWindow)
	}
	return (p.start - int64(Slack)) / 1e6, (end + int64(Slack)) / 1e6
}
