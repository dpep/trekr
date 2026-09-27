class Relation
  def load_all(limit)
    limit
  end
end

class Proxy < Relation
  def load_all(limit)
    limit
  end
end

# Prepended at runtime by something no class body says.
module FetchWarning
  def load_all(limit)
    super.tap { |records| records }
  end
end

module StaleCheck
  def extract(env)
    guarded { super }
  end

  def guarded
    yield
  end
end

class Store < Unindexed::Store
  include StaleCheck
end
