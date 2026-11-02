module Probe
  Outer::Target.class_eval do
    LIMIT = 3
    def limited; LIMIT; end
  end

  def self.build
    Outer::Target.new.real
    LIMIT
  end
end

Outer::Lorem.instance_eval do
  X = 1
end

X
