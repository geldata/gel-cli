CREATE MIGRATION m1mdwoyrh5c677pkvx57hvsxlsqiycrhijh6wyytflykey5essjiba
    ONTO initial
{
  CREATE TYPE default::Hello {
      CREATE PROPERTY world: std::str;
  };
};
