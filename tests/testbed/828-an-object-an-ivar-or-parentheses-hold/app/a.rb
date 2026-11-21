class Foo
  def foo_only; 1; end
end

class Runner
  def self.run(a)
    @w = Foo.new
    def @w.ivar_def
      foo_only
    end
    class << @w
      def ivar_singleton
        2
      end
    end
    def (a).paren_def
      3
    end
    [a].each { def it.it_def; 4; end }
    @w.ivar_def
    @w.ivar_singleton
  end
end
Runner.run(Object.new)
