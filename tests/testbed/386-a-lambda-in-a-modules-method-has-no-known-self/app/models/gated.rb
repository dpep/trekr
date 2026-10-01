module Gated
  def self.options(kwargs)
    kwargs.merge(if: [-> { feature_on? }])
  end
end
