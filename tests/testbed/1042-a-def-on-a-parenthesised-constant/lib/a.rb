module Outer
  INNER = Object.new
end

def (Outer::INNER).ix
  :i
end

LOOSE = Object.new

def (LOOSE).px
  :p
end

module App
  def self.run
    [Outer::INNER.ix, LOOSE.px]
  end
end
App.run
