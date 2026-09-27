require "forwardable"

class Engine
  def start; "started"; end
  def stop; "stopped"; end
  def rev(times); "rev" * times; end
end

class Car
  extend Forwardable

  def_delegator :@engine, :start
  def_delegator :@engine, :stop, :halt
  def_delegators :@engine, :rev

  def initialize
    @engine = Engine.new
  end

  def go
    [start, halt, rev(2)]
  end
end
