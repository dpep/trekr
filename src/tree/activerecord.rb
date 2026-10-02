# ActiveRecord's query interface as a caller sees it: what each method that
# builds a relation returns. Its source makes one with `spawn`, `clone` and
# `klass.all`, which no reading types, so `Post.where(…).order(…)` stopped
# typing after one step and the next call fell to whichever class shared the
# name — `posts.order` to OptionParser's (DEC-444).
#
# trekr reads this only to lend these return types to ActiveRecord's own
# methods of the same owner and name: nothing here is a location, and a
# method an app or another gem defines is not described by it.

module ActiveRecord
  module QueryMethods
    # `where` with no argument is the WhereChain `where.not` is called on:
    # only a call that passes one is described.
    sig { params(a: T.untyped).returns(ActiveRecord::Relation) }
    sig { params(a: T.untyped, b: T.untyped).returns(ActiveRecord::Relation) }
    sig { params(a: T.untyped, b: T.untyped, c: T.untyped).returns(ActiveRecord::Relation) }
    def where(a = nil, b = nil, c = nil); end

    # `select { |r| … }` filters the loaded records; given columns, it is a
    # relation.
    sig { params(a: T.untyped, block: NilClass).returns(ActiveRecord::Relation) }
    sig { params(a: T.untyped, b: T.untyped, block: NilClass).returns(ActiveRecord::Relation) }
    sig { params(a: T.untyped, b: T.untyped, c: T.untyped, block: NilClass).returns(ActiveRecord::Relation) }
    sig { params(block: T.proc.void).returns(Array) }
    def select(a = nil, b = nil, c = nil, &block); end

    sig { returns(ActiveRecord::Relation) }
    def includes(*); end

    sig { returns(ActiveRecord::Relation) }
    def eager_load(*); end

    sig { returns(ActiveRecord::Relation) }
    def preload(*); end

    sig { returns(ActiveRecord::Relation) }
    def references(*); end

    sig { returns(ActiveRecord::Relation) }
    def reselect(*); end

    sig { returns(ActiveRecord::Relation) }
    def group(*); end

    sig { returns(ActiveRecord::Relation) }
    def regroup(*); end

    sig { returns(ActiveRecord::Relation) }
    def order(*); end

    sig { returns(ActiveRecord::Relation) }
    def reorder(*); end

    sig { returns(ActiveRecord::Relation) }
    def unscope(*); end

    sig { returns(ActiveRecord::Relation) }
    def joins(*); end

    sig { returns(ActiveRecord::Relation) }
    def left_outer_joins(*); end

    sig { returns(ActiveRecord::Relation) }
    def left_joins(*); end

    sig { returns(ActiveRecord::Relation) }
    def rewhere(*); end

    sig { returns(ActiveRecord::Relation) }
    def invert_where(*); end

    sig { returns(ActiveRecord::Relation) }
    def and(*); end

    sig { returns(ActiveRecord::Relation) }
    def or(*); end

    sig { returns(ActiveRecord::Relation) }
    def having(*); end

    sig { returns(ActiveRecord::Relation) }
    def limit(*); end

    sig { returns(ActiveRecord::Relation) }
    def offset(*); end

    sig { returns(ActiveRecord::Relation) }
    def lock(*); end

    sig { returns(ActiveRecord::Relation) }
    def none(*); end

    sig { returns(ActiveRecord::Relation) }
    def readonly(*); end

    sig { returns(ActiveRecord::Relation) }
    def strict_loading(*); end

    sig { returns(ActiveRecord::Relation) }
    def create_with(*); end

    sig { returns(ActiveRecord::Relation) }
    def from(*); end

    sig { returns(ActiveRecord::Relation) }
    def distinct(*); end

    sig { returns(ActiveRecord::Relation) }
    def extending(*); end

    sig { returns(ActiveRecord::Relation) }
    def optimizer_hints(*); end

    sig { returns(ActiveRecord::Relation) }
    def reverse_order(*); end

    sig { returns(ActiveRecord::Relation) }
    def annotate(*); end

    sig { returns(ActiveRecord::Relation) }
    def excluding(*); end

    sig { returns(ActiveRecord::Relation) }
    def without(*); end

    sig { returns(ActiveRecord::Relation) }
    def in_order_of(*); end

    sig { returns(ActiveRecord::Relation) }
    def with(*); end

    sig { returns(ActiveRecord::Relation) }
    def with_recursive(*); end

    class WhereChain
      sig { returns(ActiveRecord::Relation) }
      def not(*); end

      sig { returns(ActiveRecord::Relation) }
      def missing(*); end

      sig { returns(ActiveRecord::Relation) }
      def associated(*); end
    end
  end

  module SpawnMethods
    sig { returns(ActiveRecord::Relation) }
    def merge(*); end

    sig { returns(ActiveRecord::Relation) }
    def except(*); end

    sig { returns(ActiveRecord::Relation) }
    def only(*); end

    sig { returns(ActiveRecord::Relation) }
    def spawn(*); end
  end

  module Scoping
    module Named
      module ClassMethods
        sig { returns(ActiveRecord::Relation) }
        def all(*); end

        sig { params(block: NilClass).returns(ActiveRecord::Relation) }
        def unscoped(&block); end
      end
    end
  end

  # A model's class methods `delegate` these to `all`, so each returns what
  # the relation's own does.
  module Querying
    # `where` with no argument is the WhereChain `where.not` is called on:
    # only a call that passes one is described.
    sig { params(a: T.untyped).returns(ActiveRecord::Relation) }
    sig { params(a: T.untyped, b: T.untyped).returns(ActiveRecord::Relation) }
    sig { params(a: T.untyped, b: T.untyped, c: T.untyped).returns(ActiveRecord::Relation) }
    def where(a = nil, b = nil, c = nil); end

    # `select { |r| … }` filters the loaded records; given columns, it is a
    # relation.
    sig { params(a: T.untyped, block: NilClass).returns(ActiveRecord::Relation) }
    sig { params(a: T.untyped, b: T.untyped, block: NilClass).returns(ActiveRecord::Relation) }
    sig { params(a: T.untyped, b: T.untyped, c: T.untyped, block: NilClass).returns(ActiveRecord::Relation) }
    sig { params(block: T.proc.void).returns(Array) }
    def select(a = nil, b = nil, c = nil, &block); end

    sig { returns(ActiveRecord::Relation) }
    def includes(*); end

    sig { returns(ActiveRecord::Relation) }
    def eager_load(*); end

    sig { returns(ActiveRecord::Relation) }
    def preload(*); end

    sig { returns(ActiveRecord::Relation) }
    def references(*); end

    sig { returns(ActiveRecord::Relation) }
    def reselect(*); end

    sig { returns(ActiveRecord::Relation) }
    def group(*); end

    sig { returns(ActiveRecord::Relation) }
    def regroup(*); end

    sig { returns(ActiveRecord::Relation) }
    def order(*); end

    sig { returns(ActiveRecord::Relation) }
    def reorder(*); end

    sig { returns(ActiveRecord::Relation) }
    def unscope(*); end

    sig { returns(ActiveRecord::Relation) }
    def joins(*); end

    sig { returns(ActiveRecord::Relation) }
    def left_outer_joins(*); end

    sig { returns(ActiveRecord::Relation) }
    def left_joins(*); end

    sig { returns(ActiveRecord::Relation) }
    def rewhere(*); end

    sig { returns(ActiveRecord::Relation) }
    def invert_where(*); end

    sig { returns(ActiveRecord::Relation) }
    def and(*); end

    sig { returns(ActiveRecord::Relation) }
    def or(*); end

    sig { returns(ActiveRecord::Relation) }
    def having(*); end

    sig { returns(ActiveRecord::Relation) }
    def limit(*); end

    sig { returns(ActiveRecord::Relation) }
    def offset(*); end

    sig { returns(ActiveRecord::Relation) }
    def lock(*); end

    sig { returns(ActiveRecord::Relation) }
    def none(*); end

    sig { returns(ActiveRecord::Relation) }
    def readonly(*); end

    sig { returns(ActiveRecord::Relation) }
    def strict_loading(*); end

    sig { returns(ActiveRecord::Relation) }
    def create_with(*); end

    sig { returns(ActiveRecord::Relation) }
    def from(*); end

    sig { returns(ActiveRecord::Relation) }
    def distinct(*); end

    sig { returns(ActiveRecord::Relation) }
    def extending(*); end

    sig { returns(ActiveRecord::Relation) }
    def optimizer_hints(*); end

    sig { returns(ActiveRecord::Relation) }
    def reverse_order(*); end

    sig { returns(ActiveRecord::Relation) }
    def annotate(*); end

    sig { returns(ActiveRecord::Relation) }
    def excluding(*); end

    sig { returns(ActiveRecord::Relation) }
    def without(*); end

    sig { returns(ActiveRecord::Relation) }
    def in_order_of(*); end

    sig { returns(ActiveRecord::Relation) }
    def with(*); end

    sig { returns(ActiveRecord::Relation) }
    def with_recursive(*); end

    sig { returns(ActiveRecord::Relation) }
    def merge(*); end

    sig { returns(ActiveRecord::Relation) }
    def except(*); end

    sig { returns(ActiveRecord::Relation) }
    def only(*); end
  end
end
