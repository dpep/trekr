class Kennel < ActiveRecord::Base
  Pt = Struct.new(:xx, :yy)
  Dt = Data.define(:dd)
  has_many :dogs

  %w[up].each do |dir|
    class_eval("def #{dir.upcase}_shout; end")
  end
end
