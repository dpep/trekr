module Declarative
  def test(name, &block)
    define_method("test_#{name.gsub(/\s+/, '_')}", &block)
  end

  def before(&)
    define_method(:setup_before, &)
  end

  def configure(&block)
    class_exec(&block)
  end

  def later(&block)
    yield
  end
end

class Base
  extend Declarative

  def helper(x)
    x
  end

  def wrap
    yield
  end

  def self.setting(x)
    x
  end
end

class WidgetTest < Base
  test "it works" do
    helper(1)
  end

  before do
    [1].each { |n| helper(n) }
  end

  configure do
    setting(1)
    helper(2)
  end

  later do
    helper(3)
  end

  test "in a block of its own" do
    wrap { helper(4) }
    Clock.hold { helper(5) }
  end
end

class Clock
  def self.hold
    yield
  end
end
