module Ns
  class Inner
  end

  def self.all
    constants.map { |c| const_get(c) }
  end
end
