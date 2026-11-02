module Mixin
  def mixed
  end
end

class Probe
  Target.class_eval do
    include Mixin
  end

  def mix
    Target.new.mixed
  end
end
