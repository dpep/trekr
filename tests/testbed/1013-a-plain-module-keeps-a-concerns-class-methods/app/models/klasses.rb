class KC
  include Outer
  has_inner
end

class KD
  include Dependent
  has_inner
end

class KE
  include Inner
  has_inner
end
