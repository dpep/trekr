module Probe
  Target.class_eval do
    DEFAULT_LIMIT = 3
    def limited; DEFAULT_LIMIT; end
  end

  def self.build
    Target.new.real
    DEFAULT_LIMIT
  end
end
